//! Unix process-group ownership with nonblocking output and bounded retirement.
//!
//! The child leader is observed with `WNOWAIT` until group termination has been
//! attempted successfully. Keeping the leader unreaped reserves its PID/PGID;
//! an external process-wide child reaper violates that ownership contract. If
//! that loss is detected, this owner never signals the possibly recycled group.
//! Process groups are not a sandbox: descendants can escape them with `setsid`.

use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;

use super::{CommandSpec, StdioMode};

const OWNER_LIMIT: usize = 32;

// Reservations cover both active owners and children whose cleanup has not yet
// completed. Retirement never creates threads, grows a collection, or waits.
static OWNERS: Mutex<[Slot; OWNER_LIMIT]> = Mutex::new([const { Slot::Empty }; OWNER_LIMIT]);

enum Slot {
    Empty,
    Reserved,
    Retired(OwnedChild),
}

/// An admitted child and its reserved retirement slot.
///
/// Drop performs one nonblocking cleanup attempt. A still-owned child is retained
/// in its pre-reserved slot and polled on later admissions. Uninterruptible kernel
/// tasks or lost child identities can therefore exhaust the finite admission
/// capacity; they never cause an unbounded wait or an unbounded owner collection.
pub struct Process {
    slot: usize,
    owner: Option<OwnedChild>,
}

struct OwnedChild {
    child: Child,
    pid: libc::pid_t,
    exit: Option<ExitStatus>,
    termination_attempted: bool,
    group_terminated: bool,
    direct_stopped: bool,
    reaped: bool,
    identity_lost: bool,
}

impl Process {
    /// Creates a fresh command with inherited environment and directory. The
    /// mode is the sole authority for child stdio configuration.
    pub fn spawn(spec: CommandSpec, mode: StdioMode) -> io::Result<Self> {
        let mut command = Command::new(spec.get_program());
        command.args(spec.get_args());
        let slot = reserve_slot()?;
        command.process_group(0);
        match mode {
            StdioMode::Capture => {
                command.stdin(Stdio::piped()).stdout(Stdio::piped());
            }
            StdioMode::Diagnostics => {
                command.stdin(Stdio::null()).stdout(Stdio::null());
            }
        }
        command.stderr(Stdio::piped());
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                release_slot(slot);
                return Err(error);
            }
        };
        // Unix child IDs originate as positive pid_t values in the OS; Child
        // exposes the same bits as u32, so this cast cannot truncate a valid PID.
        let pid = child.id() as libc::pid_t;
        let process = Self {
            slot,
            owner: Some(OwnedChild {
                child,
                pid,
                exit: None,
                termination_attempted: false,
                group_terminated: false,
                direct_stopped: false,
                reaped: false,
                identity_lost: false,
            }),
        };
        // From this point all setup failures go through Process::drop, including
        // failures to configure either pipe. The reservation predates the spawn.
        let owner = process.owner.as_ref().expect("new child owner exists");
        if let Some(stdout) = &owner.child.stdout {
            nonblocking(stdout.as_raw_fd())?;
        }
        if let Some(stderr) = &owner.child.stderr {
            nonblocking(stderr.as_raw_fd())?;
        }
        Ok(process)
    }

    /// Observes the direct child without reaping or releasing its PID reservation.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.owner_mut().observe_exit()
    }

    pub fn read_stdout(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match &mut self.owner_mut().child.stdout {
            Some(pipe) => pipe.read(buffer),
            None => Ok(0),
        }
    }

    pub fn read_stderr(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match &mut self.owner_mut().child.stderr {
            Some(pipe) => pipe.read(buffer),
            None => Ok(0),
        }
    }

    /// Closes the stdin lifetime token and signals the group before leader reap.
    pub fn terminate(&mut self) -> io::Result<()> {
        self.owner_mut().terminate()
    }

    /// One nonblocking cleanup observation. Call terminate first; a successful
    /// result means the direct child was reaped, not that escaped descendants
    /// were contained. Output remains readable until this owner is dropped.
    pub fn cleanup_complete(&mut self) -> io::Result<bool> {
        self.owner_mut().cleanup_complete()
    }

    fn owner_mut(&mut self) -> &mut OwnedChild {
        self.owner.as_mut().expect("live Process retains its child")
    }
}

impl OwnedChild {
    fn identity_error() -> io::Error {
        io::Error::other(
            "child identity lost before owned cleanup; refusing recycled process-group signal",
        )
    }

    fn observe_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.identity_lost {
            return Err(Self::identity_error());
        }
        if self.reaped {
            return Ok(self.exit);
        }
        // SAFETY: siginfo_t is a C output record; all-zero storage is valid before
        // waitid initializes it, and it contains no Rust references or ownership.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: pid names our unreaped child, info is writable for the complete
        // siginfo_t, and WNOWAIT explicitly retains the child's PID reservation.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.identity_lost = true;
                return Err(Self::identity_error());
            }
            return Err(error);
        }
        // SAFETY: waitid succeeded with WEXITED and zero-initialized output. A
        // zero si_pid means no status is available; no union status is read yet.
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        // SAFETY: the successful WEXITED event has a nonzero child PID, so its
        // child-status payload is initialized for CLD_EXITED/KILLED/DUMPED.
        let status = unsafe { info.si_status() };
        let raw = match info.si_code {
            libc::CLD_EXITED => (status & 0xff) << 8,
            libc::CLD_KILLED => status & 0x7f,
            libc::CLD_DUMPED => (status & 0x7f) | 0x80,
            _ => return Err(io::Error::other("unexpected non-exit child observation")),
        };
        let exit = ExitStatus::from_raw(raw);
        self.exit = Some(exit);
        Ok(Some(exit))
    }

    fn terminate(&mut self) -> io::Result<()> {
        self.termination_attempted = true;
        self.child.stdin.take();
        if self.identity_lost {
            return Err(Self::identity_error());
        }
        if self.reaped {
            return Ok(());
        }
        // Even an exited leader remains unreaped until its original group is
        // handled. ECHILD forbids either signal because identity may be recycled.
        self.observe_exit()?;
        let group_result = if self.group_terminated {
            Ok(())
        } else {
            self.stop_original_group()
        };
        // A child can join another process group without creating a new session.
        // Always stop its owned PID too; never signal its potentially new group.
        // Attempt this even when signaling the original group failed.
        let direct_result = if self.direct_stopped {
            Ok(())
        } else {
            self.stop_direct_child()
        };
        group_result.and(direct_result)
    }

    fn stop_original_group(&mut self) -> io::Result<()> {
        // SAFETY: the child was admitted with process_group(0), is exclusively
        // owned and unreaped, and observe_exit verified that ownership. Its PID
        // cannot be recycled before cleanup_complete reaps the direct child.
        let result = unsafe { libc::kill(-self.pid, libc::SIGKILL) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                // Darwin's group-signal implementation skips zombies and can
                // return EPERM for a zombie-only group. Do not confuse that with
                // permission refusal for live descendants: require a complete,
                // bounded native inventory proving every member is a zombie.
                #[cfg(target_os = "macos")]
                if error.raw_os_error() == Some(libc::EPERM)
                    && self.observe_exit()?.is_some()
                    && darwin_group_is_zombie_only(self.pid)
                {
                    self.group_terminated = true;
                    return Ok(());
                }
                return Err(error);
            }
        }
        self.group_terminated = true;
        Ok(())
    }

    fn stop_direct_child(&mut self) -> io::Result<()> {
        // Revalidate child ownership before signaling, including after a failed
        // original-group attempt. An exit observation retains the PID via WNOWAIT.
        self.observe_exit()?;
        // SAFETY: self.pid still identifies our exclusively owned unreaped child,
        // regardless of any process-group changes made by that child.
        if unsafe { libc::kill(self.pid, libc::SIGKILL) } != 0 {
            let error = io::Error::last_os_error();
            // A platform can reject a signal to an already-exited process. Only
            // a fresh owned exit observation justifies accepting that rejection.
            if self.observe_exit()?.is_none() {
                return Err(error);
            }
        }
        self.direct_stopped = true;
        Ok(())
    }

    fn cleanup_complete(&mut self) -> io::Result<bool> {
        if !self.termination_attempted {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminate must precede child cleanup observation",
            ));
        }
        if self.identity_lost {
            return Err(Self::identity_error());
        }
        if self.reaped {
            return Ok(true);
        }
        if !self.group_terminated || !self.direct_stopped {
            return Err(io::Error::other(
                "process-group or direct-child termination has not completed",
            ));
        }
        if self.observe_exit()?.is_none() {
            return Ok(false);
        }
        // Child::try_wait is nonblocking. Unlike wait_with_output/wait it neither
        // closes stdin early nor waits for descendants holding output pipes.
        match self.child.try_wait() {
            Ok(Some(exit)) => {
                self.exit = Some(exit);
                self.reaped = true;
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(error) => {
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.identity_lost = true;
                    Err(Self::identity_error())
                } else {
                    Err(error)
                }
            }
        }
    }

    fn poll_cleanup(&mut self) -> bool {
        // Each operation is a single nonblocking kernel observation/signal. An
        // interrupted call is left for the next admission, never retried here.
        self.terminate().is_ok() && self.cleanup_complete().unwrap_or(false)
    }

    fn close_pipes(&mut self) {
        self.child.stdin.take();
        self.child.stdout.take();
        self.child.stderr.take();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let Some(mut owner) = self.owner.take() else {
            return;
        };
        let complete = owner.poll_cleanup();
        owner.close_pipes();
        let mut slots = OWNERS.lock().unwrap_or_else(|error| error.into_inner());
        slots[self.slot] = if complete {
            Slot::Empty
        } else {
            Slot::Retired(owner)
        };
    }
}

fn reserve_slot() -> io::Result<usize> {
    // The finite sweep also services children retired by earlier requests. A
    // Reserved marker protects a temporarily extracted owner from other callers.
    for index in 0..OWNER_LIMIT {
        let retired = {
            let mut slots = OWNERS.lock().unwrap_or_else(|error| error.into_inner());
            if matches!(slots[index], Slot::Retired(_)) {
                match std::mem::replace(&mut slots[index], Slot::Reserved) {
                    Slot::Retired(owner) => Some(owner),
                    _ => unreachable!("slot checked while mutex held"),
                }
            } else {
                None
            }
        };
        if let Some(mut owner) = retired {
            let complete = owner.poll_cleanup();
            let mut slots = OWNERS.lock().unwrap_or_else(|error| error.into_inner());
            slots[index] = if complete {
                Slot::Empty
            } else {
                Slot::Retired(owner)
            };
        }
    }
    let mut slots = OWNERS.lock().unwrap_or_else(|error| error.into_inner());
    let Some(index) = slots.iter().position(|slot| matches!(slot, Slot::Empty)) else {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "child-process owner capacity exhausted by active or unreaped children",
        ));
    };
    slots[index] = Slot::Reserved;
    Ok(index)
}

fn release_slot(index: usize) {
    OWNERS.lock().unwrap_or_else(|error| error.into_inner())[index] = Slot::Empty;
}

fn nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fd is borrowed from a live, uniquely owned child output pipe; fcntl
    // does not consume it. F_GETFL takes no variadic pointer argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the same live pipe descriptor is used, with its existing flags
    // preserved and only O_NONBLOCK added. F_SETFL receives the required integer.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn darwin_group_is_zombie_only(pgid: libc::pid_t) -> bool {
    // libproc's public PROC_PGRP_ONLY constant (sys/proc_info.h). The fixed
    // capacity bounds both memory and native calls; a full buffer is ambiguous.
    const PROC_PGRP_ONLY: u32 = 2;
    let mut pids = [0 as libc::pid_t; 256];
    let bytes = std::mem::size_of_val(&pids);
    // SAFETY: the array is writable for exactly bytes bytes, the flavor is the
    // documented process-group selector, and no pointers escape the native call.
    let received = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            pgid as u32,
            pids.as_mut_ptr().cast(),
            bytes as libc::c_int,
        )
    };
    // libproc returns zero for both an empty list and native lookup failure.
    // Neither proves this EPERM case safe. A completely filled buffer may have
    // truncated live members; never accept it as a complete group inventory.
    if received <= 0 || received as usize >= bytes {
        return false;
    }
    let received = received as usize;
    if !received.is_multiple_of(std::mem::size_of::<libc::pid_t>()) {
        return false;
    }
    for &pid in &pids[..received / std::mem::size_of::<libc::pid_t>()] {
        if pid <= 0 {
            return false;
        }
        // SAFETY: proc_bsdshortinfo is an integer/byte C output record; zero
        // initialization is valid and proc_pidinfo gets its complete writable size.
        let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info);
        // SAFETY: info is writable for size bytes. The documented SHORTBSDINFO
        // flavor with arg=1 includes zombies; the result is used only if full.
        let received = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDT_SHORTBSDINFO,
                1,
                (&mut info as *mut libc::proc_bsdshortinfo).cast(),
                size as libc::c_int,
            )
        };
        if received != size as libc::c_int
            || info.pbsi_pid != pid as u32
            || info.pbsi_pgid != pgid as u32
            || info.pbsi_status != libc::SZOMB
        {
            return false;
        }
    }
    // Zombies cannot create new descendants. This is process-group cleanup for
    // cooperative children, not containment against external session members
    // deliberately joining the group after the snapshot or escaped descendants.
    true
}
