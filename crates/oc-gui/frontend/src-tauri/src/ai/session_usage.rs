//! Bounded, atomic session accounting, independent of transcript retention.
//! Only an admitted owner lease may commit provider attempts. Closed leases
//! cannot write again, even when a client reuses its cancellation UUID.

use super::limits::Price;
use super::sessions::Store;
use super::timing::{AttemptStats, TurnStats, TurnStatsHandle};
use crate::private_fs;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

const FILE: &str = "usage.json";
const MAX_BYTES: u64 = 256 * 1024;
const MAX_RECEIPTS: usize = 8;
const MAX_ACTIVE: usize = 64;
const MAX_ATTEMPTS: usize = 4;
const MAX_CURRENCIES: usize = 16;
static ACTIVE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);
static RUN: LazyLock<Result<String, AccountingError>> = LazyLock::new(nonce);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
/// Stable error code for session accounting.
pub enum AccountingCode {
    #[serde(rename = "accounting_storage")]
    Storage,
    #[serde(rename = "accounting_session_missing")]
    SessionMissing,
    #[serde(rename = "accounting_entropy")]
    Entropy,
    #[serde(rename = "accounting_overflow")]
    Overflow,
    #[serde(rename = "accounting_capacity")]
    Capacity,
    #[serde(rename = "accounting_currency_capacity")]
    CurrencyCapacity,
    #[serde(rename = "accounting_attempt_capacity")]
    AttemptCapacity,
    #[serde(rename = "accounting_format")]
    Format,
    #[serde(rename = "accounting_request")]
    Request,
    #[serde(rename = "accounting_busy")]
    Busy,
    #[serde(rename = "accounting_session_changed")]
    SessionChanged,
    #[serde(rename = "accounting_owner_closed")]
    OwnerClosed,
    #[serde(rename = "accounting_owner_changed")]
    OwnerChanged,
    #[serde(rename = "accounting_conflict")]
    Conflict,
    #[serde(rename = "accounting_cost")]
    Cost,
}
impl AccountingCode {
    /// Wire spelling of the code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "accounting_storage",
            Self::SessionMissing => "accounting_session_missing",
            Self::Entropy => "accounting_entropy",
            Self::Overflow => "accounting_overflow",
            Self::Capacity => "accounting_capacity",
            Self::CurrencyCapacity => "accounting_currency_capacity",
            Self::AttemptCapacity => "accounting_attempt_capacity",
            Self::Format => "accounting_format",
            Self::Request => "accounting_request",
            Self::Busy => "accounting_busy",
            Self::SessionChanged => "accounting_session_changed",
            Self::OwnerClosed => "accounting_owner_closed",
            Self::OwnerChanged => "accounting_owner_changed",
            Self::Conflict => "accounting_conflict",
            Self::Cost => "accounting_cost",
        }
    }
}
impl std::fmt::Display for AccountingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Why an accounting request was admitted.
pub enum Purpose {
    Chat,
    Title,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RetryDisposition {
    #[default]
    Terminal,
    Io,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// A failed accounting operation with its stable code.
pub struct AccountingError {
    pub code: AccountingCode,
    pub message: String,
    #[serde(skip)]
    retry: RetryDisposition,
}
impl AccountingError {
    fn new(code: AccountingCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry: RetryDisposition::Terminal,
        }
    }
    pub(super) fn storage(error: impl std::fmt::Display) -> Self {
        Self::new(AccountingCode::Storage, error.to_string())
    }
    pub(super) fn io(error: std::io::Error) -> Self {
        let retry = match error.kind() {
            std::io::ErrorKind::InvalidInput
            | std::io::ErrorKind::InvalidData
            | std::io::ErrorKind::Unsupported => RetryDisposition::Terminal,
            _ => RetryDisposition::Io,
        };
        Self {
            code: AccountingCode::Storage,
            message: error.to_string(),
            retry,
        }
    }
    pub(super) fn format(error: impl std::fmt::Display) -> Self {
        Self::new(AccountingCode::Format, error.to_string())
    }
    pub(super) fn missing(error: impl std::fmt::Display) -> Self {
        Self::new(AccountingCode::SessionMissing, error.to_string())
    }
}
fn admit_active(active: &mut HashSet<String>, token: &str) -> Result<(), AccountingError> {
    if active.len() >= MAX_ACTIVE {
        return Err(AccountingError::new(
            AccountingCode::Busy,
            "Too many accounting owners are active",
        ));
    }
    active.insert(token.to_owned());
    Ok(())
}
fn nonce() -> Result<String, AccountingError> {
    let mut bytes = [0; 16];
    SystemRandom::new().fill(&mut bytes).map_err(|_| {
        AccountingError::new(
            AccountingCode::Entropy,
            "Could not create accounting ownership identity",
        )
    })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn run() -> Result<&'static str, AccountingError> {
    RUN.as_ref().map(String::as_str).map_err(Clone::clone)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
/// Spend total for one currency.
pub struct CurrencyTotal {
    pub amount: f64,
    pub attempts: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
/// Aggregated usage and cost for a session.
pub struct Totals {
    pub requests: u64,
    pub attempts: u64,
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cached_input: u64,
    /// Inclusive input only from attempts that reported a cache field.
    pub cache_input: u64,
    pub failed_attempts: u64,
    pub unknown_price_attempts: u64,
    pub unreported_usage_attempts: u64,
    pub interrupted_requests: u64,
    pub legacy_unknown: bool,
    pub ttft_ms_sum: u64,
    pub ttft_count: u64,
    pub decode_tps_sum: f64,
    pub decode_tps_count: u64,
    pub costs: BTreeMap<String, CurrencyTotal>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Public accounting snapshot for one session.
pub struct SessionUsageSnapshot {
    pub session_id: String,
    pub incarnation: String,
    pub revision: u64,
    pub totals: Totals,
    pub pending_requests: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_turn: Option<TurnStats>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Receipt {
    token: String,
    run: String,
    purpose: Purpose,
    #[serde(default)]
    currencies: BTreeSet<String>,
    attempts: Vec<AttemptStats>,
    #[serde(default)]
    admission_order: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Ledger {
    v: u32,
    incarnation: String,
    revision: u64,
    totals: Totals,
    last_turn: Option<TurnStats>,
    #[serde(default)]
    last_turn_order: u64,
    receipts: BTreeMap<String, Receipt>,
}
impl Ledger {
    fn new(legacy_unknown: bool) -> Result<Self, AccountingError> {
        Ok(Self {
            v: 1,
            incarnation: nonce()?,
            revision: 0,
            totals: Totals {
                legacy_unknown,
                ..Default::default()
            },
            last_turn: None,
            last_turn_order: 0,
            receipts: BTreeMap::new(),
        })
    }
    fn snapshot(&self, id: &str) -> SessionUsageSnapshot {
        SessionUsageSnapshot {
            session_id: id.into(),
            incarnation: self.incarnation.clone(),
            revision: self.revision,
            totals: self.totals.clone(),
            pending_requests: self.receipts.len(),
            last_turn: self.last_turn.clone(),
        }
    }
    fn advance(&mut self) -> Result<(), AccountingError> {
        increment(&mut self.revision, 1)
    }
    fn recover(&mut self) -> Result<bool, AccountingError> {
        let active = ACTIVE.lock().map_err(AccountingError::storage)?;
        let current = run()?;
        let before = self.receipts.len();
        self.receipts
            .retain(|_, receipt| receipt.run == current && active.contains(&receipt.token));
        let lost = before - self.receipts.len();
        if lost > 0 {
            increment(&mut self.totals.interrupted_requests, lost as u64)?;
            self.advance()?;
        }
        Ok(lost > 0)
    }
}
fn increment(value: &mut u64, amount: u64) -> Result<(), AccountingError> {
    *value = value.checked_add(amount).ok_or_else(|| {
        AccountingError::new(
            AccountingCode::Overflow,
            "Session accounting counter overflow",
        )
    })?;
    Ok(())
}
fn read(dir: &Path) -> Result<Option<Ledger>, AccountingError> {
    let path = dir.join(FILE);
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself, so metadata rejects links and devices.
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    }
    let file = match options.open(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        result => result.map_err(AccountingError::io)?,
    };
    let meta = file.metadata().map_err(AccountingError::io)?;
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
    if reparse || !meta.is_file() {
        return Err(AccountingError::new(
            AccountingCode::Storage,
            "Accounting must be a regular file",
        ));
    }
    if meta.len() > MAX_BYTES {
        return Err(AccountingError::new(
            AccountingCode::Format,
            "Accounting exceeds its storage budget",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(AccountingError::io)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(AccountingError::new(
            AccountingCode::Format,
            "Accounting exceeds its storage budget",
        ));
    }
    let ledger: Ledger = serde_json::from_slice(&bytes)
        .map_err(|error| AccountingError::new(AccountingCode::Format, error.to_string()))?;
    validate_ledger(&ledger)?;
    Ok(Some(ledger))
}
fn validate_ledger(ledger: &Ledger) -> Result<(), AccountingError> {
    let mut currencies: BTreeSet<&str> = ledger.totals.costs.keys().map(String::as_str).collect();
    for receipt in ledger.receipts.values() {
        currencies.extend(receipt.currencies.iter().map(String::as_str));
    }
    if currencies.len() > MAX_CURRENCIES {
        return Err(AccountingError::new(
            AccountingCode::CurrencyCapacity,
            "Session currency capacity exceeded",
        ));
    }
    if ledger.v != 1
        || ledger.incarnation.len() != 32
        || !ledger.totals.decode_tps_sum.is_finite()
        || ledger.totals.decode_tps_sum < 0.0
        || ledger.totals.costs.len() > MAX_CURRENCIES
        || ledger
            .totals
            .costs
            .iter()
            .any(|(key, value)| !currency(key) || !value.amount.is_finite() || value.amount < 0.0)
        || ledger
            .last_turn
            .as_ref()
            .is_some_and(|turn| !valid_turn(turn))
        || ledger.receipts.iter().any(|(key, receipt)| {
            key.is_empty()
                || key.len() > 100
                || receipt.token.len() != 32
                || receipt.run.len() != 32
                || receipt.currencies.len() > MAX_CURRENCIES
                || receipt.currencies.iter().any(|key| !currency(key))
                || receipt.attempts.iter().any(|attempt| {
                    !valid_turn(&attempt.stats) || validate_price(attempt.price.as_ref()).is_err()
                })
        })
        || ledger.receipts.len() > MAX_RECEIPTS
        || ledger
            .receipts
            .values()
            .any(|receipt| receipt.attempts.len() > MAX_ATTEMPTS)
    {
        return Err(AccountingError::new(
            AccountingCode::Format,
            "Unsupported or oversized session accounting",
        ));
    }
    Ok(())
}
fn write(dir: &Path, ledger: &Ledger) -> Result<(), AccountingError> {
    validate_ledger(ledger)?;
    let bytes = serde_json::to_vec(ledger).map_err(AccountingError::format)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(AccountingError::new(
            AccountingCode::Format,
            "Session accounting exceeds its storage budget",
        ));
    }
    private_fs::write_atomic(&dir.join(FILE), &bytes).map_err(AccountingError::io)
}
pub(super) fn initialize(dir: &Path) -> Result<(), AccountingError> {
    write(dir, &Ledger::new(false)?)
}
pub(super) fn snapshot_at(
    dir: &Path,
    id: &str,
) -> Result<Option<SessionUsageSnapshot>, AccountingError> {
    let Some(mut ledger) = read(dir)? else {
        return Ok(None);
    };
    if ledger.recover()? {
        write(dir, &ledger)?;
    }
    Ok(Some(ledger.snapshot(id)))
}

#[derive(Debug, Clone, Serialize)]
/// Result of one accounting commit, including durability.
pub struct AccountingUpdate {
    pub session_id: String,
    pub incarnation: String,
    pub revision: u64,
    pub snapshot: SessionUsageSnapshot,
    pub saved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AccountingError>,
}
#[derive(Debug)]
struct LeaseState {
    closed: bool,
    final_attempts: Option<Vec<AttemptStats>>,
    retry_started: bool,
    update: AccountingUpdate,
}
#[derive(Debug)]
/// Exclusive owner lease for one admitted accounting request.
pub struct SessionLease {
    store: Store,
    session_id: String,
    request_id: String,
    token: String,
    state: Mutex<LeaseState>,
}
impl SessionLease {
    /// Admit a request, creating the session ledger when needed.
    pub fn admit(
        store: Store,
        session_id: &str,
        request_id: &str,
        purpose: Purpose,
    ) -> Result<Arc<Self>, AccountingError> {
        if request_id.is_empty() || request_id.len() > 100 {
            return Err(AccountingError::new(
                AccountingCode::Request,
                "Invalid accounting request identity",
            ));
        }
        let token = nonce()?;
        {
            let mut active = ACTIVE.lock().map_err(AccountingError::storage)?;
            admit_active(&mut active, &token)?;
        }
        let admitted = store.with_usage(session_id, |dir| {
            let mut ledger = match read(dir)? {
                Some(ledger) => ledger,
                None => Ledger::new(true)?,
            };
            ledger.recover()?;
            if ledger.receipts.len() >= MAX_RECEIPTS || ledger.receipts.contains_key(request_id) {
                return Err(AccountingError::new(
                    AccountingCode::Busy,
                    "This session already owns that request or has too many active requests",
                ));
            }
            increment(&mut ledger.totals.requests, 1)?;
            ledger.receipts.insert(
                request_id.into(),
                Receipt {
                    token: token.clone(),
                    run: run()?.into(),
                    purpose,
                    currencies: BTreeSet::new(),
                    attempts: Vec::new(),
                    admission_order: ledger.totals.requests,
                },
            );
            ledger.advance()?;
            write(dir, &ledger)?;
            Ok(ledger.snapshot(session_id))
        });
        let snapshot = match admitted {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Ok(mut active) = ACTIVE.lock() {
                    active.remove(&token);
                }
                return Err(error);
            }
        };
        Ok(Arc::new(Self {
            store,
            session_id: session_id.into(),
            request_id: request_id.into(),
            token,
            state: Mutex::new(LeaseState {
                closed: false,
                final_attempts: None,
                retry_started: false,
                update: update(snapshot, true, None),
            }),
        }))
    }
    /// Latest accounting update for this lease.
    pub fn update(&self) -> AccountingUpdate {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .update
            .clone()
    }
    /// Verify the session ledger still has this lease incarnation.
    pub fn verify_session(&self) -> Result<(), AccountingError> {
        let incarnation = self.update().incarnation;
        self.store.with_usage(&self.session_id, |dir| {
            let ledger = read(dir)?.ok_or_else(|| {
                AccountingError::new(
                    AccountingCode::SessionChanged,
                    "Session accounting was removed",
                )
            })?;
            if ledger.incarnation != incarnation {
                return Err(AccountingError::new(
                    AccountingCode::SessionChanged,
                    "Session incarnation changed",
                ));
            }
            Ok(())
        })
    }
    fn reserve_price(&self, price: Option<&Price>) -> Result<(), AccountingError> {
        validate_price(price)?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed || state.final_attempts.is_some() {
            return Err(AccountingError::new(
                AccountingCode::OwnerClosed,
                "Accounting owner is closed",
            ));
        }
        let result = self.store.with_usage(&self.session_id, |dir| {
            let mut ledger = read(dir)?.ok_or_else(|| {
                AccountingError::new(AccountingCode::SessionChanged, "Accounting was removed")
            })?;
            let receipt = ledger.receipts.get(&self.request_id).ok_or_else(|| {
                AccountingError::new(AccountingCode::OwnerClosed, "Accounting owner is absent")
            })?;
            if ledger.incarnation != state.update.incarnation
                || receipt.token != self.token
                || receipt.run != run()?
            {
                return Err(AccountingError::new(
                    AccountingCode::OwnerChanged,
                    "Accounting owner changed",
                ));
            }
            if receipt.attempts.len() >= MAX_ATTEMPTS {
                return Err(AccountingError::new(
                    AccountingCode::AttemptCapacity,
                    "Provider attempt capacity exceeded",
                ));
            }
            if let Some(price) = price {
                let mut currencies: BTreeSet<_> = ledger.totals.costs.keys().cloned().collect();
                for receipt in ledger.receipts.values() {
                    currencies.extend(receipt.currencies.iter().cloned());
                }
                currencies.insert(price.currency.clone());
                if currencies.len() > MAX_CURRENCIES {
                    return Err(AccountingError::new(
                        AccountingCode::CurrencyCapacity,
                        "Session currency capacity exceeded",
                    ));
                }
                if ledger
                    .receipts
                    .get_mut(&self.request_id)
                    .expect("validated owner")
                    .currencies
                    .insert(price.currency.clone())
                {
                    ledger.advance()?;
                    write(dir, &ledger)?;
                }
            }
            Ok(ledger.snapshot(&self.session_id))
        });
        match result {
            Ok(snapshot) => {
                state.update = update(snapshot, true, None);
                Ok(())
            }
            Err(error) => {
                state.update.saved = false;
                state.update.error = Some(error.clone());
                Err(error)
            }
        }
    }
    /// Commit attempts and optionally finish the request.
    pub fn commit(&self, attempts: &[AttemptStats], finish: bool) -> AccountingUpdate {
        self.commit_using(attempts, finish, write)
    }
    fn commit_using(
        &self,
        attempts: &[AttemptStats],
        finish: bool,
        publish: impl Fn(&Path, &Ledger) -> Result<(), AccountingError>,
    ) -> AccountingUpdate {
        self.publish_attempts(attempts, finish, false, publish)
    }
    fn retry_using(
        &self,
        publish: impl Fn(&Path, &Ledger) -> Result<(), AccountingError>,
    ) -> AccountingUpdate {
        let attempts = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .final_attempts
            .clone();
        match attempts {
            Some(attempts) => self.publish_attempts(&attempts, true, true, publish),
            None => self.update(),
        }
    }
    fn start_retry(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed || state.final_attempts.is_none() || state.retry_started {
            return false;
        }
        state.retry_started = true;
        true
    }
    fn publish_attempts(
        &self,
        attempts: &[AttemptStats],
        finish: bool,
        retry: bool,
        publish: impl Fn(&Path, &Ledger) -> Result<(), AccountingError>,
    ) -> AccountingUpdate {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed || (state.final_attempts.is_some() && !retry) {
            return state.update.clone();
        }
        if finish && state.final_attempts.is_none() {
            if attempts.len() > MAX_ATTEMPTS
                || attempts.iter().any(|attempt| {
                    !valid_turn(&attempt.stats) || validate_price(attempt.price.as_ref()).is_err()
                })
            {
                state.closed = true;
                state.update.saved = false;
                state.update.error = Some(AccountingError::new(
                    if attempts.len() > MAX_ATTEMPTS {
                        AccountingCode::AttemptCapacity
                    } else {
                        AccountingCode::Request
                    },
                    "Invalid final accounting payload",
                ));
                if let Ok(mut active) = ACTIVE.lock() {
                    active.remove(&self.token);
                }
                return state.update.clone();
            }
            state.final_attempts = Some(attempts.to_vec());
        }
        let incarnation = state.update.incarnation.clone();
        let result = self.store.with_usage(&self.session_id, |dir| {
            let mut ledger = read(dir)?.ok_or_else(|| {
                AccountingError::new(
                    AccountingCode::SessionChanged,
                    "Session accounting was removed while a request was running",
                )
            })?;
            if ledger.incarnation != incarnation {
                return Err(AccountingError::new(
                    AccountingCode::SessionChanged,
                    "Session accounting incarnation changed",
                ));
            }
            let receipt = ledger.receipts.get(&self.request_id).ok_or_else(|| {
                AccountingError::new(
                    AccountingCode::OwnerClosed,
                    "Accounting owner is no longer admitted",
                )
            })?;
            if receipt.token != self.token || receipt.run != run()? {
                return Err(AccountingError::new(
                    AccountingCode::OwnerChanged,
                    "Accounting owner identity changed",
                ));
            }
            let purpose = receipt.purpose;
            let admission_order = receipt.admission_order;
            let prior = receipt.attempts.clone();
            if attempts.len() > MAX_ATTEMPTS
                || attempts.len() < prior.len()
                || prior
                    .iter()
                    .zip(attempts)
                    .any(|(left, right)| left != right)
            {
                return Err(AccountingError::new(
                    AccountingCode::Conflict,
                    "Provider attempt finalization conflicts with committed usage",
                ));
            }
            if attempts
                .iter()
                .enumerate()
                .any(|(index, attempt)| attempt.ordinal as usize != index)
            {
                return Err(AccountingError::new(
                    AccountingCode::Conflict,
                    "Provider attempts are not in their owned order",
                ));
            }
            if attempts.len() == prior.len() && !finish {
                return Ok(ledger.snapshot(&self.session_id));
            }
            for attempt in &attempts[prior.len()..] {
                add_attempt(&mut ledger.totals, attempt, purpose == Purpose::Chat)?;
            }
            if purpose == Purpose::Chat
                && admission_order >= ledger.last_turn_order
                && let Some(attempt) = attempts.last()
            {
                ledger.last_turn = Some(attempt.stats.clone());
                ledger.last_turn_order = admission_order;
            }
            if finish {
                ledger.receipts.remove(&self.request_id);
            } else {
                ledger
                    .receipts
                    .get_mut(&self.request_id)
                    .expect("validated owner")
                    .attempts = attempts.to_vec();
            }
            ledger.advance()?;
            publish(dir, &ledger)?;
            Ok(ledger.snapshot(&self.session_id))
        });
        match result {
            Ok(snapshot) => state.update = update(snapshot, true, None),
            Err(error) => {
                state.update.saved = false;
                state.update.error = Some(error);
            }
        }
        if finish
            && (state.update.saved
                || state
                    .update
                    .error
                    .as_ref()
                    .is_none_or(|error| error.retry == RetryDisposition::Terminal))
        {
            state.closed = true;
            if let Ok(mut active) = ACTIVE.lock() {
                active.remove(&self.token);
            }
        }
        state.update.clone()
    }
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE.lock() {
            active.remove(&self.token);
        }
    }
}
fn update(
    snapshot: SessionUsageSnapshot,
    saved: bool,
    error: Option<AccountingError>,
) -> AccountingUpdate {
    AccountingUpdate {
        session_id: snapshot.session_id.clone(),
        incarnation: snapshot.incarnation.clone(),
        revision: snapshot.revision,
        snapshot,
        saved,
        error,
    }
}
fn add_attempt(
    totals: &mut Totals,
    attempt: &AttemptStats,
    chat: bool,
) -> Result<(), AccountingError> {
    let turn = &attempt.stats;
    if !valid_turn(turn) {
        return Err(AccountingError::new(
            AccountingCode::Format,
            "Invalid provider usage record",
        ));
    }
    validate_price(attempt.price.as_ref())?;
    increment(&mut totals.attempts, 1)?;
    increment(&mut totals.input, turn.usage.input)?;
    increment(&mut totals.output, turn.usage.output)?;
    increment(&mut totals.reasoning, turn.usage.reasoning)?;
    if let Some(cached) = turn.usage.cached_input {
        increment(&mut totals.cached_input, cached.min(turn.usage.input))?;
        increment(&mut totals.cache_input, turn.usage.input)?;
    }
    if turn.failed {
        increment(&mut totals.failed_attempts, 1)?;
    }
    if !attempt.usage_reported {
        increment(&mut totals.unreported_usage_attempts, 1)?;
    }
    if turn.usage.input > 0 || turn.usage.output > 0 {
        match &turn.cost {
            Some(cost) => {
                if !cost.amount.is_finite() || cost.amount < 0.0 || cost.currency.len() != 3 {
                    return Err(AccountingError::new(
                        AccountingCode::Cost,
                        "Invalid provider cost estimate",
                    ));
                }
                if !totals.costs.contains_key(&cost.currency)
                    && totals.costs.len() >= MAX_CURRENCIES
                {
                    return Err(AccountingError::new(
                        AccountingCode::CurrencyCapacity,
                        "Session currency capacity exceeded",
                    ));
                }
                let bucket = totals.costs.entry(cost.currency.clone()).or_default();
                let amount = bucket.amount + cost.amount;
                if !amount.is_finite() {
                    return Err(AccountingError::new(
                        AccountingCode::Overflow,
                        "Session cost estimate overflow",
                    ));
                }
                bucket.amount = amount;
                increment(&mut bucket.attempts, 1)?;
            }
            None => increment(&mut totals.unknown_price_attempts, 1)?,
        }
    }
    if chat {
        if let Some(ttft) = turn.ttft_ms {
            increment(&mut totals.ttft_ms_sum, ttft)?;
            increment(&mut totals.ttft_count, 1)?;
        }
        if let Some(tps) = turn
            .decode_tps
            .filter(|value| value.is_finite() && *value >= 0.0)
        {
            let sum = totals.decode_tps_sum + tps;
            if !sum.is_finite() {
                return Err(AccountingError::new(
                    AccountingCode::Overflow,
                    "Session timing total overflow",
                ));
            }
            totals.decode_tps_sum = sum;
            increment(&mut totals.decode_tps_count, 1)?;
        }
    }
    Ok(())
}

/// Existing turn fields remain flat; accounting never appears in the footer
/// stored inside SessionUsageSnapshot, avoiding recursive wire types.
#[derive(Clone, Serialize)]
pub struct StatsEvent {
    #[serde(flatten)]
    pub turn: TurnStats,
    pub client_request_id: String,
    pub purpose: Purpose,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounting: Option<AccountingUpdate>,
}
/// Commit a turn attempts off the async runtime.
pub async fn checkpoint(stats: &TurnStatsHandle, finish: bool) -> Option<AccountingUpdate> {
    let lease = stats.accounting()?;
    let attempts = stats.attempts();
    let worker = lease.clone();
    match tauri::async_runtime::spawn_blocking(move || worker.commit(&attempts, finish)).await {
        Ok(update) => Some(update),
        Err(error) => {
            let mut state = lease
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.update.saved = false;
            state.update.error = Some(AccountingError::storage(error));
            if finish {
                state.closed = true;
                if let Ok(mut active) = ACTIVE.lock() {
                    active.remove(&lease.token);
                }
            }
            Some(state.update.clone())
        }
    }
}
/// Final publication owns its immutable payload until acknowledgement. The
/// reporter captures presentation context only, never a timing handle or lease.
/// Commit and publish the final accounting update.
pub async fn checkpoint_final(
    stats: &TurnStatsHandle,
    app: &tauri::AppHandle,
    client_request_id: &str,
    purpose: Purpose,
) -> Option<AccountingUpdate> {
    let lease = stats.accounting()?;
    let update = checkpoint(stats, true).await?;
    if lease.start_retry() {
        let app = app.clone();
        let turn = stats.snapshot();
        let client_request_id = client_request_id.to_string();
        tauri::async_runtime::spawn(async move {
            use tauri::Emitter;
            let mut backoff = RetryBackoff::default();
            loop {
                tokio::time::sleep(backoff.next_delay()).await;
                let worker = lease.clone();
                let update =
                    match tauri::async_runtime::spawn_blocking(move || worker.retry_using(write))
                        .await
                    {
                        Ok(update) => update,
                        Err(error) => {
                            let mut state = lease
                                .state
                                .lock()
                                .unwrap_or_else(|error| error.into_inner());
                            state.closed = true;
                            state.update.saved = false;
                            state.update.error = Some(AccountingError::storage(error));
                            if let Ok(mut active) = ACTIVE.lock() {
                                active.remove(&lease.token);
                            }
                            state.update.clone()
                        }
                    };
                if update.saved
                    || update
                        .error
                        .as_ref()
                        .is_none_or(|error| error.retry == RetryDisposition::Terminal)
                {
                    let _ = app.emit(
                        "ai_stats",
                        StatsEvent {
                            turn,
                            client_request_id,
                            purpose,
                            accounting: Some(update),
                        },
                    );
                    break;
                }
            }
        });
    }
    Some(update)
}
#[derive(Default)]
struct RetryBackoff {
    step: u32,
}
impl RetryBackoff {
    fn next_delay(&mut self) -> std::time::Duration {
        let seconds = 1u64.checked_shl(self.step.min(5)).unwrap_or(30).min(30);
        self.step = self.step.saturating_add(1);
        std::time::Duration::from_secs(seconds)
    }
}

/// A fresh nonce for a title-generation request.
pub fn title_id() -> Result<String, AccountingError> {
    nonce()
}

/// Preserve a paid reply while reporting accounting durability independently.
pub fn annotate_reply(reply: &mut serde_json::Value, update: &AccountingUpdate) {
    if !update.saved {
        reply["accounting_status"] =
            serde_json::to_value(update).unwrap_or(serde_json::Value::Null);
    }
}

fn currency(value: &str) -> bool {
    value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_uppercase())
}
fn valid_turn(turn: &TurnStats) -> bool {
    turn.model.len() <= 512
        && turn.wire.len() <= 32
        && turn.request_id.as_ref().is_none_or(|id| id.len() <= 512)
        && turn.cost.as_ref().is_none_or(|cost| {
            currency(&cost.currency) && cost.amount.is_finite() && cost.amount >= 0.0
        })
        && [turn.decode_tps, turn.wall_tps]
            .into_iter()
            .flatten()
            .all(|value| value.is_finite() && value >= 0.0)
}
fn validate_price(price: Option<&Price>) -> Result<(), AccountingError> {
    if let Some(price) = price {
        let bound = f64::MAX / u64::MAX as f64 / 4.0;
        if !currency(&price.currency)
            || [Some(price.input), Some(price.output), price.cached_input]
                .into_iter()
                .flatten()
                .any(|value| !value.is_finite() || value < 0.0 || value > bound)
        {
            return Err(AccountingError::new(
                AccountingCode::Cost,
                "Invalid provider pricing",
            ));
        }
    }
    Ok(())
}
/// Validate provider/model accounting identity and price.
pub async fn prepare_attempt(
    stats: &TurnStatsHandle,
    provider: &str,
    model: &str,
    price: Option<Price>,
) -> Result<(), AccountingError> {
    if provider.len() > 128 || model.len() > 512 {
        return Err(AccountingError::new(
            AccountingCode::Request,
            "Provider accounting identity exceeds its supported length",
        ));
    }
    if stats.attempts().len() >= MAX_ATTEMPTS {
        return Err(AccountingError::new(
            AccountingCode::AttemptCapacity,
            "Provider attempt capacity exceeded",
        ));
    }
    validate_price(price.as_ref())?;
    if let Some(lease) = stats.accounting() {
        let reserved = price.clone();
        tauri::async_runtime::spawn_blocking(move || lease.reserve_price(reserved.as_ref()))
            .await
            .map_err(AccountingError::storage)??;
    }
    stats.set_next_price(price);
    Ok(())
}

#[cfg(test)]
#[path = "session_usage/retry_tests.rs"]
mod retry_tests;
#[cfg(test)]
#[path = "session_usage/tests.rs"]
mod tests;

pub(super) fn verify_at(dir: &Path, incarnation: Option<&str>) -> Result<(), AccountingError> {
    if let Some(expected) = incarnation {
        let ledger = read(dir)?.ok_or_else(|| {
            AccountingError::new(
                AccountingCode::SessionChanged,
                "Session accounting was removed",
            )
        })?;
        if ledger.incarnation != expected {
            return Err(AccountingError::new(
                AccountingCode::SessionChanged,
                "Session incarnation changed",
            ));
        }
    }
    Ok(())
}
