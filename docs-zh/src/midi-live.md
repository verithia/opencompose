# 实时 MIDI

`oc_engine::midi_io` 负责控制器输入和 MIDI 输出：直通（thru）、生成的音符以及 24-PPQN 时钟。它还提供一个用于测试的确定性假后端。

## 后端

- `midir` 0.11 负责平台 I/O：macOS 上是 CoreMIDI，Windows 上是 WinMM，Linux 上是 ALSA。Unix 上可以使用虚拟端口，CoreMIDI 回环测试就用到了它们。
- `rtrb` 0.3 提供 MIDI 线程与音频回调之间预先分配的单生产者、单消费者队列。
- `FakeBackend` 实现同一个 `MidiBackend` trait，用于测试和无硬件监听。

`MidiBackend` 是唯一的平台边界：

```rust
fn list_inputs(&self) -> Result<Vec<MidiPortInfo>, MidiError>;
fn list_outputs(&self) -> Result<Vec<MidiPortInfo>, MidiError>;
fn open_input(&mut self, port: &MidiPortInfo) -> Result<InputId, MidiError>;
fn open_output(&mut self, port: &MidiPortInfo) -> Result<OutputId, MidiError>;
fn close_input(&mut self, input: InputId);
fn close_output(&mut self, output: OutputId);
fn send(&mut self, output: OutputId, message: &[u8]) -> Result<(), MidiError>;
fn poll(&mut self, input: InputId, out: &mut [MidiInputEvent]) -> Result<usize, MidiError>;
```

错误的类型是 `MidiError`：后端不可用、找不到端口、端口已打开、权限被拒绝、已断开、输入缓冲区溢出、无效消息以及一般 I/O 错误。后端错误在边界处完成映射，缺少设备绝不会导致崩溃。

## 输入路径

1. `midir` 的输入回调把每条 1 到 3 字节的消息复制进一个预先分配的 1024 槽环形缓冲区。它不解析、不加锁、不分配内存。如果环形缓冲区满了，回调会停止为该连接排队，因为中间出现缺口可能会丢失音符释放。之后 `poll` 先返回已排队的完整事件，再返回输入缓冲区溢出错误。
2. MIDI 线程在 `MidiBackend::poll` 中取空环形缓冲区。它把 note on/off、复音与通道触后、CC、音色切换、弯音、时钟以及 start/continue/stop 解析成固定大小的 `LiveEvent` 值。
3. 同一线程把事件推入 `midi_io::event_queue`，这是一个有界的 `rtrb` 队列，生产者一端是 `LiveEventProducer`。
4. 音频回调持有 `LiveEventConsumer`。它每个块最多取出 256 个事件，通过 `send_midi` 应用到 `OpenSynth`。回调不分配内存、不加锁，每个块的工作量有上限。向已满的队列推入事件会立即把事件退回，绝不阻塞。此时 `ocp play` 会以 "Live MIDI synth queue overflowed" 停止实时输入。

力度为零的 note-on 在到达乐器之前会变成 note-off。

## 输出路径

- 直通：`MidiSession::forward` 把轮询到的输入事件逐字节发送到所选输出（`pack_into`）。
- 生成的音符：`OutputScheduler` 读取解释器的 `MidiEvent` 列表，在播放光标到达某个事件的采样时间时发送它。14 位 CC 会拆成两个线路事件。
- 时钟：`MidiClock` 根据播放位置每个四分音符发送 24 个 tick。`set_clock_enabled` 可以在不重启会话的情况下开关时钟。

调度器运行在 MIDI 线程上，CoreMIDI、WinMM 和 ALSA 的发送在这里可以阻塞。时钟抖动取决于轮询间隔（CLI 中为 1 ms），与音频回调无关。

## CLI

```sh
ocp midi list
ocp midi monitor --input "IAC Driver Bus 1"
ocp play song.ocp --midi-in "IAC Driver Bus 1"
ocp play song.ocp --midi-in keys --midi-out synth
ocp play song.ocp --midi-in keys --midi-out synth --midi-clock --midi-thru
```

`ocp midi list` 打印带索引的输入和输出端口。凡是需要指定端口的地方，都可以使用名称或索引。

`ocp midi monitor` 为每个事件打印时间戳：

```text
[ 12.345678] note on   ch=1 C4   vel=100
```

`ocp play` 先离线渲染工程，再通过应用音频设置（`~/.opencompose/audio.json`）中保存的输出设备和周期进行流式播放。实时输入演奏工程中的第一个 OpenSynth 预设；如果没有，则使用默认音色。使用 `--midi-out` 时，工程事件在到时时发送。`--midi-clock` 开启 24-PPQN 时钟，`--midi-thru` 把控制器转发到输出。

`ocp play` 使用逐轨渲染路径，所以内置、OpenSynth 和 SoundFont 轨道都可以播放。CLAP 和 VST3 乐器轨道需要离线插件渲染路径，不属于实时播放。MIDI 时钟只跟随工程的基础速度，不跟随速度映射。

## 在应用中

在 [偏好设置](preferences.md) 的 **音频** 页面中选择 MIDI 输入。相关命令位于 `crates/oc-gui/frontend/src-tauri/src/midi_live.rs`：

- `list_midi_ports` 返回输入和输出端口，包括索引、名称和一个不透明的 id。没有 MIDI 后端时，它返回 `supported: false` 和一条消息，页面会在行内显示该消息，绝不弹出对话框。
- `get_midi_config` 和 `set_midi_config` 读取或应用 `{ input, monitor }`，以及可选的实时 MPE 区域（`mpe`）。
- `get_midi_activity` 返回最近一个通道事件、事件计数、线程状态和当前错误。监听期间页面以 4 Hz 轮询它。

状态保存在 `AppStateInner::midi_live` 中：选择、输入线程和无锁的活动探针。它与 `AudioState` 中的音频设置相互独立。选择保存在 `~/.opencompose/midi.json`，例如 `{ "input": "Keys", "monitor": true }`。文件缺失或损坏时使用默认值。写入失败会在行内显示。

MIDI 线程把解析好的 `LiveEvent` 推入打开的 OpenSynth 试听；正在录制 MIDI 片段时，也同时推入录制。只有试听打开时（音色编辑器的键盘或预设试听）你才能听到输入。否则页面会显示 **打开合成器音色编辑器即可听到实时输入**，活动显示照常更新。这样输出流始终只有一个所有者，也不会打断走带播放。

即使 **实时输入监听** 处于关闭状态，MIDI 录制也会打开所选输入，并在录制结束后再次释放它。

### 线程

每个启用的输入有一个 MIDI 线程。线程持有 `MidirBackend` 和 `MidiSession`，每 1 ms 轮询一次，写入一个固定的 64 事件缓冲区，在该线程上解析，并用原子操作更新活动探针。

随后线程把 `Command::Live` 推入试听共享的生产者槽 `live_synth::Route`（`Arc<Mutex<Option<CommandProducer>>>`）。互斥锁只在推入时，或在试听开始、停止时交换生产者时持有。命令环形缓冲区有 256 个槽。它接纳的每个 note-on 都会为对应的释放预留一个槽，所以缓冲区满时，MIDI 一侧丢弃的是新的按键，释放仍会送达。

音频回调持有消费者，每个块最多取出 256 条命令并应用到 `OpenSynth`。回调从不分配内存、加锁、创建线程或阻塞。

输入停止时，线程会为每个仍按住的音符发送 note-off，并释放按住的延音踏板，所以试听合成器上不会有声音残留。

启动、停止或更换设备时，界面会在持有 `AppState` 锁的情况下等待 MIDI 线程结束。这是安全的，因为 MIDI 线程从不获取这个锁。

### 行为

- 每次打开 **音频** 页面以及按下刷新按钮（↻）时，都会重新读取端口列表。没有后台热插拔监视。已保存但已消失的设备保持选中，并标注为“（不可用）”，直到你更改它。
- 如果设备打开失败（端口缺失、权限问题、没有后端），选择仍会保存，错误在行内显示。没有选择输入时，**实时输入监听** 不可用。
- 启动时，前端 store（`stores/midiLive.ts`）恢复已保存的选择。如果 `monitor` 为 true，它会先把设备与当时存在的端口核对，再恢复监听。
- 活动显示最近的音符和力度，或最近的 CC、音色切换或弯音事件。实时时钟 tick 绝不会替换最近事件。

## 无硬件测试

用 `cargo test -p oc-engine midi_io` 测试核心部分，用 `cargo test -p oc-gui --lib midi_live` 测试界面桥接。界面测试使用 `FakeBackend` 和一个记录用的接收端，所以不需要设备：

- `midi_live::tests::port_entries_preserve_index_name_and_id`
- `midi_live::tests::preferences_round_trip_and_missing_file_uses_defaults`
- `midi_live::tests::corrupt_preferences_fall_back_to_defaults`
- `midi_live::tests::selection_persists_without_opening_a_device`
- `midi_live::tests::absent_backend_is_reported_without_panicking`
- `midi_live::tests::input_thread_routes_events_and_releases_after_stop_or_overflow`
- `midi_live::tests::missing_port_is_reported_by_the_runtime`
- `midi_live::tests::activity_ignores_clock_and_keeps_the_last_note`
- 前端：`__tests__/stores/midiLive.test.ts` 和 `__tests__/components/ToolbarMidiLive.test.tsx`

核心 `midi_io` 测试涵盖：

- 消息往返与格式错误的输入；
- 队列上限、顺序和每块预算；
- 24-PPQN 时钟和调度器的到时事件；
- 通过 `session::tests::*` 测试 `FakeBackend`；
- `midir_backend::tests::missing_port_is_a_typed_error`；
- 仅限 macOS 的 `coremidi_virtual_loopback`，虚拟端口不可用时跳过。

## 需要真实硬件的部分

- 枚举实体控制器、热插拔与断开，以及权限提示。
- 通过试听合成器听到输入，并通过真实音频接口和 DIN 或 USB MIDI 测量延迟与抖动。
- WinMM 和 ALSA 的行为（回环测试只在 CoreMIDI 上运行）。
- 与外部设备的时钟同步，以及与真实合成器的直通保真度。
