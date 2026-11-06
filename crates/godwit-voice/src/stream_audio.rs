// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A screen share's sound: what the shared app (or every app but Godwit)
//! plays, as 10 ms frames of 48 kHz interleaved stereo.
//!
//! Godwit's own sound (the call, its alerts) is always left out, or the
//! viewers would hear the call echoed back through the stream.
//!
//! - Windows: WASAPI process loopback (Windows 10 2004+). A shared app's
//!   window records that app's process tree; a whole screen records every
//!   process but Godwit's own tree.
//! - Linux: one PulseAudio record stream per app's playback stream
//!   (`direct_on_input`, as pavucontrol's level meters), mixed here, skipping
//!   Godwit's own streams. That's the Pulse protocol, which PipeWire serves
//!   too (pipewire-pulse), so plain PulseAudio and PipeWire both work: a hard
//!   rule for Linux audio in Godwit. The desktop portal's picker gives no
//!   app, so a Linux share always records every app but Godwit.
//!
//! The official Stoat client asks the browser for the share's audio with
//! `restrictOwnAudio` and publishes it as LiveKit's `ScreenShareAudio`
//! (for-web `rtc/state.tsx`); the call does the same with these frames.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// Samples (both channels) in one 10 ms frame.
pub const FRAME: usize = (SAMPLE_RATE / 100 * CHANNELS) as usize;

/// What to record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Every app but Godwit.
    AllButGodwit,
    /// One app (a process id) and its child processes.
    App(u32),
}

/// What a share of the picked source records: the window's app when a
/// window is shared (and it isn't Godwit's), every app but Godwit otherwise.
pub fn target_for_share() -> Target {
    #[cfg(windows)]
    if let Some(source) = super::screen::picked().filter(|s| s.window)
        && let Some(pid) = super::screen::window_process(source.id)
        && pid != std::process::id()
    {
        return Target::App(pid);
    }
    Target::AllButGodwit
}

/// A running capture; dropping it stops it and waits for its thread.
pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Collects samples into whole 10 ms frames for `out`.
struct Framer<F: FnMut(&[i16])> {
    pending: Vec<i16>,
    out: F,
}

impl<F: FnMut(&[i16])> Framer<F> {
    fn new(out: F) -> Self {
        Self {
            pending: Vec::with_capacity(FRAME * 4),
            out,
        }
    }

    fn push(&mut self, samples: &[i16]) {
        self.pending.extend_from_slice(samples);
        let whole = self.pending.len() / FRAME * FRAME;
        for frame in self.pending[..whole].as_chunks::<FRAME>().0 {
            (self.out)(frame);
        }
        self.pending.drain(..whole);
    }
}

/// Starts recording `target`; `frames` gets each 10 ms frame, on the
/// capture's own thread. Errs at once when this system can't record apps
/// (no Pulse server, a Windows older than 10 2004, …).
pub fn start(target: Target, frames: impl FnMut(&[i16]) + Send + 'static) -> Result<Capture> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<()>>(1);
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("stream-audio".into())
        .spawn(move || platform::run(target, flag, ready_tx, Framer::new(frames)))?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(Capture {
            stop,
            thread: Some(thread),
        }),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        Err(_) => {
            let _ = thread.join();
            Err(anyhow::anyhow!("the stream audio thread ended"))
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::SyncSender;
    use std::time::Duration;

    use anyhow::{Context, Result, anyhow};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Media::Audio::{
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS,
        AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
        IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient,
        IAudioClient, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
    };
    use windows::Win32::System::Com::StructuredStorage::{
        PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
    };
    use windows::Win32::System::Com::{BLOB, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::Win32::System::Variant::VT_BLOB;
    use windows::core::{Interface, implement};

    use super::{CHANNELS, Framer, SAMPLE_RATE, Target};

    /// Tells the waiting thread the activation finished. WASAPI wants an
    /// agile handler (it's called on a thread of its own): windows-rs objects
    /// are agile already; naming IAgileObject again corrupted the heap.
    #[implement(IActivateAudioInterfaceCompletionHandler)]
    struct Activated(SyncSender<()>);

    impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
        fn ActivateCompleted(
            &self,
            _: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
        ) -> windows::core::Result<()> {
            let _ = self.0.send(());
            Ok(())
        }
    }

    /// The loopback client for `target`.
    fn activate(target: Target) -> Result<IAudioClient> {
        let (pid, mode) = match target {
            Target::AllButGodwit => (std::process::id(), PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE),
            Target::App(pid) => (pid, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE),
        };
        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: mode,
                },
            },
        };
        // A VT_BLOB pointing at `params`, which outlives the call. Never
        // dropped: windows-rs drops a PROPVARIANT with PropVariantClear,
        // which would free `params` (on the stack) and corrupt the heap.
        let variant = std::mem::ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: (&raw mut params).cast(),
                        },
                    },
                }),
            },
        });
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let handler: IActivateAudioInterfaceCompletionHandler = Activated(done_tx).into();
        // SAFETY: `variant` and `params` live until the activation has
        // completed (we wait for it below, or give up and leak nothing).
        let operation = unsafe {
            ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(&raw const *variant),
                &handler,
            )
        }
        .context("process loopback needs Windows 10 version 2004 or later")?;
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow!("the audio loopback didn't start"))?;
        let mut result = windows::core::HRESULT(0);
        let mut client = None;
        // SAFETY: the operation completed; both out-pointers are valid.
        unsafe { operation.GetActivateResult(&mut result, &mut client)? };
        result.ok().context("starting the audio loopback")?;
        client
            .ok_or_else(|| anyhow!("no audio loopback client"))?
            .cast::<IAudioClient>()
            .context("audio loopback client")
    }

    pub(super) fn run<F: FnMut(&[i16])>(
        target: Target,
        stop: Arc<AtomicBool>,
        ready: SyncSender<Result<()>>,
        mut framer: Framer<F>,
    ) {
        // SAFETY: this thread's own COM apartment, closed below.
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let started = start_client(target);
        let (client, capture, event) = match started {
            Ok(parts) => {
                let _ = ready.send(Ok(()));
                parts
            }
            Err(error) => {
                let _ = ready.send(Err(error));
                if com.is_ok() {
                    // SAFETY: matches the CoInitializeEx above.
                    unsafe { CoUninitialize() };
                }
                return;
            }
        };
        let mut samples = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            // SAFETY: `event` is ours until CloseHandle below.
            if unsafe { WaitForSingleObject(event, 100) } != WAIT_OBJECT_0 {
                continue;
            }
            if read_packets(&capture, &mut samples).is_err() {
                break;
            }
            framer.push(&samples);
            samples.clear();
        }
        // SAFETY: the client and event are ours; COM closes last.
        unsafe {
            let _ = client.Stop();
            let _ = CloseHandle(event);
            drop(capture);
            drop(client);
            if com.is_ok() {
                CoUninitialize();
            }
        }
    }

    fn start_client(target: Target) -> Result<(IAudioClient, IAudioCaptureClient, HANDLE)> {
        let client = activate(target)?;
        // Process loopback can't report a mix format: ask for ours, and let
        // WASAPI convert (AUTOCONVERTPCM).
        let block = (CHANNELS * 2) as u16;
        let format = WAVEFORMATEX {
            wFormatTag: 1, // WAVE_FORMAT_PCM
            nChannels: CHANNELS as u16,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // SAFETY: COM calls on this thread's client; `format` outlives them.
        unsafe {
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK
                        | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                    200_000, // 20 ms, in 100 ns units
                    0,
                    &format,
                    None,
                )
                .context("setting up the audio loopback")?;
            let event = CreateEventW(None, false, false, windows::core::PCWSTR::null())?;
            let ready = client
                .SetEventHandle(event)
                .and_then(|()| client.GetService::<IAudioCaptureClient>())
                .and_then(|capture| client.Start().map(|()| capture));
            match ready {
                Ok(capture) => Ok((client, capture, event)),
                Err(error) => {
                    let _ = CloseHandle(event);
                    Err(error).context("starting the audio loopback")
                }
            }
        }
    }

    /// Everything the loopback has, appended to `out` (silence as zeros).
    fn read_packets(capture: &IAudioCaptureClient, out: &mut Vec<i16>) -> windows::core::Result<()> {
        // SAFETY: each buffer is read before it's released, within its size.
        unsafe {
            while capture.GetNextPacketSize()? > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let count = frames as usize * CHANNELS as usize;
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    out.resize(out.len() + count, 0);
                } else {
                    out.extend_from_slice(std::slice::from_raw_parts(data.cast::<i16>(), count));
                }
                capture.ReleaseBuffer(frames)?;
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::{HashMap, VecDeque};
    use std::ffi::CString;
    use std::io::BufReader;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::SyncSender;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, anyhow};
    use pulseaudio::protocol;

    use super::{CHANNELS, FRAME, Framer, SAMPLE_RATE, Target};
    use crate::sync::lock;

    /// How often new or ended app streams are looked for.
    const RESCAN: Duration = Duration::from_secs(2);
    /// At most this much is kept per app (a slow tick drops the oldest).
    const MAX_BUFFERED: usize = FRAME * 20;

    type Buffer = Arc<Mutex<VecDeque<i16>>>;

    /// A plain blocking connection for listing playback streams: the
    /// pulseaudio crate's client has no call for it, its protocol layer does.
    struct Lister {
        socket: BufReader<std::os::unix::net::UnixStream>,
        version: u16,
        seq: u32,
    }

    impl Lister {
        fn connect() -> Result<Self> {
            let path = pulseaudio::socket_path_from_env().ok_or_else(|| anyhow!("no PulseAudio server"))?;
            let cookie = pulseaudio::cookie_path_from_env()
                .and_then(|p| std::fs::read(p).ok())
                .unwrap_or_default();
            let socket = std::os::unix::net::UnixStream::connect(path).context("connecting to PulseAudio")?;
            socket.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut lister = Self {
                socket: BufReader::new(socket),
                version: protocol::MAX_VERSION,
                seq: 0,
            };
            let auth: protocol::AuthReply = lister.ask(protocol::Command::Auth(protocol::AuthParams {
                version: protocol::MAX_VERSION,
                supports_shm: false,
                supports_memfd: false,
                cookie,
            }))?;
            lister.version = protocol::MAX_VERSION.min(auth.version);
            let mut props = protocol::Props::new();
            props.set(protocol::Prop::ApplicationName, c"Godwit (stream audio)");
            if let Ok(pid) = CString::new(std::process::id().to_string()) {
                props.set(protocol::Prop::ApplicationProcessId, pid.as_c_str());
            }
            let _: protocol::SetClientNameReply = lister.ask(protocol::Command::SetClientName(props))?;
            Ok(lister)
        }

        fn ask<R: protocol::CommandReply>(&mut self, command: protocol::Command) -> Result<R> {
            let seq = self.seq;
            self.seq += 1;
            protocol::write_command_message(self.socket.get_mut(), seq, &command, self.version)?;
            let (reply_seq, reply) = protocol::read_reply_message::<R>(&mut self.socket, self.version)?;
            if reply_seq != seq {
                return Err(anyhow!("PulseAudio answered out of order"));
            }
            Ok(reply)
        }

        /// The playback streams to record: (index, owning process id).
        fn inputs(&mut self) -> Result<Vec<(u32, Option<u32>)>> {
            let list: protocol::SinkInputInfoList = self.ask(protocol::Command::GetSinkInputInfoList)?;
            Ok(list
                .into_iter()
                .map(|input| {
                    let pid = input
                        .props
                        .get(protocol::Prop::ApplicationProcessId)
                        .and_then(|raw| std::str::from_utf8(raw).ok())
                        .and_then(|s| s.trim_end_matches('\0').parse().ok());
                    (input.index, pid)
                })
                .collect())
        }
    }

    /// Whether a playback stream is wanted for `target`.
    fn wanted(target: Target, pid: Option<u32>) -> bool {
        let own = std::process::id();
        match target {
            Target::AllButGodwit => pid != Some(own),
            Target::App(app) => pid == Some(app),
        }
    }

    /// Adds `from` into `into`, clipping.
    fn mix(into: &mut [i16], from: impl Iterator<Item = i16>) {
        for (a, b) in into.iter_mut().zip(from) {
            *a = a.saturating_add(b);
        }
    }

    pub(super) fn run<F: FnMut(&[i16])>(
        target: Target,
        stop: Arc<AtomicBool>,
        ready: SyncSender<Result<()>>,
        mut framer: Framer<F>,
    ) {
        let connected = Lister::connect().and_then(|lister| {
            pulseaudio::Client::from_env(c"Godwit (stream audio)")
                .map(|client| (lister, client))
                .map_err(|e| anyhow!("connecting to PulseAudio: {e}"))
        });
        let (mut lister, client) = match connected {
            Ok(parts) => {
                let _ = ready.send(Ok(()));
                parts
            }
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        let mut streams: HashMap<u32, (pulseaudio::RecordStream, Buffer)> = HashMap::new();
        let mut next_scan = Instant::now();
        let mut tick = Instant::now();
        let mut frame = vec![0i16; FRAME];
        while !stop.load(Ordering::Relaxed) {
            if Instant::now() >= next_scan {
                next_scan = Instant::now() + RESCAN;
                match lister.inputs() {
                    Ok(inputs) => rescan(&client, target, &inputs, &mut streams),
                    Err(error) => {
                        eprintln!("error: listing apps' sound: {error:#}");
                        break;
                    }
                }
            }
            // One 10 ms frame mixed from every app with a frame's worth.
            tick += Duration::from_millis(10);
            if let Some(wait) = tick.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            } else {
                tick = Instant::now();
            }
            frame.fill(0);
            let mut any = false;
            for (_, buffer) in streams.values() {
                let mut buffer = lock(buffer);
                if buffer.len() >= FRAME {
                    mix(&mut frame, buffer.drain(..FRAME));
                    any = true;
                }
            }
            if any {
                framer.push(&frame);
            }
        }
        for (_, (stream, _)) in streams.drain() {
            let _ = futures_executor::block_on(stream.delete());
        }
    }

    /// Records new app streams and drops ended ones.
    fn rescan(
        client: &pulseaudio::Client,
        target: Target,
        inputs: &[(u32, Option<u32>)],
        streams: &mut HashMap<u32, (pulseaudio::RecordStream, Buffer)>,
    ) {
        let gone: Vec<u32> = streams
            .keys()
            .filter(|index| !inputs.iter().any(|(i, _)| i == *index))
            .copied()
            .collect();
        for index in gone {
            if let Some((stream, _)) = streams.remove(&index) {
                let _ = futures_executor::block_on(stream.delete());
            }
        }
        for &(index, pid) in inputs {
            if streams.contains_key(&index) || !wanted(target, pid) {
                continue;
            }
            match record(client, index) {
                Ok(recorded) => {
                    streams.insert(index, recorded);
                }
                Err(error) => eprintln!("error: recording an app's sound: {error:#}"),
            }
        }
    }

    /// A record stream on one app's playback stream, into a buffer.
    fn record(client: &pulseaudio::Client, index: u32) -> Result<(pulseaudio::RecordStream, Buffer)> {
        let buffer: Buffer = Arc::new(Mutex::new(VecDeque::with_capacity(MAX_BUFFERED)));
        let into = buffer.clone();
        let sink = move |bytes: &[u8]| {
            let mut buffer = lock(&into);
            buffer.extend(bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)));
            let over = buffer.len().saturating_sub(MAX_BUFFERED);
            buffer.drain(..over);
        };
        let params = protocol::RecordStreamParams {
            sample_spec: protocol::SampleSpec {
                format: protocol::SampleFormat::S16Le,
                channels: CHANNELS as u8,
                sample_rate: SAMPLE_RATE,
            },
            channel_map: protocol::ChannelMap::new([
                protocol::ChannelPosition::FrontLeft,
                protocol::ChannelPosition::FrontRight,
            ]),
            // The server records the stream's sink monitor, this stream only.
            direct_on_input_index: Some(index),
            buffer_attr: protocol::stream::BufferAttr {
                fragment_size: (FRAME * 2 * 2) as u32, // 20 ms of bytes
                ..Default::default()
            },
            flags: protocol::stream::StreamFlags {
                adjust_latency: true,
                ..Default::default()
            },
            props: {
                let mut props = protocol::Props::new();
                props.set(protocol::Prop::MediaName, c"Screen share sound");
                props
            },
            ..Default::default()
        };
        let stream = futures_executor::block_on(client.create_record_stream(params, sink))
            .map_err(|e| anyhow!("PulseAudio record stream: {e}"))?;
        Ok((stream, buffer))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn godwit_is_left_out_and_one_app_is_only_itself() {
            let own = std::process::id();
            assert!(!wanted(Target::AllButGodwit, Some(own)));
            assert!(wanted(Target::AllButGodwit, Some(own + 1)));
            assert!(wanted(Target::AllButGodwit, None));
            assert!(wanted(Target::App(42), Some(42)));
            assert!(!wanted(Target::App(42), Some(43)));
            assert!(!wanted(Target::App(42), None));
        }

        #[test]
        fn mixing_adds_and_clips() {
            let mut frame = [1000i16, i16::MAX - 5, -3];
            mix(&mut frame, [24i16, 100, -4].into_iter());
            assert_eq!(frame, [1024, i16::MAX, -7]);
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::SyncSender;

    use anyhow::Result;

    use super::{Framer, Target};

    pub(super) fn run<F: FnMut(&[i16])>(_: Target, _: Arc<AtomicBool>, ready: SyncSender<Result<()>>, _: Framer<F>) {
        let _ = ready.send(Err(anyhow::anyhow!("sharing sound isn't supported here yet")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_leave_in_whole_ten_millisecond_frames() {
        let mut sizes = Vec::new();
        let mut framer = Framer::new(|frame: &[i16]| sizes.push(frame.len()));
        framer.push(&vec![0; FRAME / 2]);
        framer.push(&vec![0; FRAME * 2]);
        assert_eq!(framer.pending.len(), FRAME / 2);
        drop(framer);
        assert_eq!(sizes, [FRAME, FRAME]);
    }
}
