use backtrace::{self, Backtrace};
use gpui::{AppContext};

use release_channel::ReleaseChannel;
use std::{
    sync::{atomic::Ordering},
};
use std::{panic, sync::atomic::AtomicU32, thread};

static PANIC_COUNT: AtomicU32 = AtomicU32::new(0);

pub fn init_panic_hook() {
    panic::set_hook(Box::new(move |info| {
        let prior_panic_count = PANIC_COUNT.fetch_add(1, Ordering::SeqCst);
        if prior_panic_count > 0 {
            // Give the panic-ing thread time to write the panic file
            loop {
                std::thread::yield_now();
            }
        }

        let thread = thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");

        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.clone()))
            .unwrap_or_else(|| "Box<Any>".to_string());

        if *release_channel::RELEASE_CHANNEL == ReleaseChannel::Dev {
            let location = info.location().unwrap();
            let backtrace = Backtrace::new();
            eprintln!(
                "Thread {:?} panicked with {:?} at {}:{}:{}\n{:?}",
                thread_name,
                payload,
                location.file(),
                location.line(),
                location.column(),
                backtrace,
            );
            std::process::exit(-1);
        }

        let backtrace = Backtrace::new();
        let mut backtrace = backtrace
            .frames()
            .iter()
            .flat_map(|frame| {
                frame
                    .symbols()
                    .iter()
                    .filter_map(|frame| Some(format!("{:#}", frame.name()?)))
            })
            .collect::<Vec<_>>();

        // Strip out leading stack frames for rust panic-handling.
        if let Some(ix) = backtrace
            .iter()
            .position(|name| name == "rust_begin_unwind")
        {
            backtrace.drain(0..=ix);
        }

        std::process::abort();
    }));
}

pub fn init(
    _cx: &mut AppContext,
) {
    #[cfg(target_os = "macos")]
    monitor_main_thread_hangs(_cx);
}

#[cfg(target_os = "macos")]
pub fn monitor_main_thread_hangs(
    cx: &AppContext,
) {
    // This is too noisy to ship to stable for now.
    if !matches!(
        ReleaseChannel::global(cx),
        ReleaseChannel::Dev | ReleaseChannel::Nightly | ReleaseChannel::Preview
    ) {
        return;
    }

    use nix::sys::signal::{
        sigaction, SaFlags, SigAction, SigHandler, SigSet,
        Signal::{self, SIGUSR2},
    };

    use parking_lot::Mutex;

    use http::Method;
    use std::{
        ffi::c_int,
        sync::{mpsc, OnceLock},
        time::Duration,
    };

    use nix::sys::pthread;

    let foreground_executor = cx.foreground_executor();
    let background_executor = cx.background_executor();

    // Initialize SIGUSR2 handler to send a backrace to a channel.
    let (backtrace_tx, backtrace_rx) = mpsc::channel();
    static BACKTRACE: Mutex<Vec<backtrace::Frame>> = Mutex::new(Vec::new());
    static BACKTRACE_SENDER: OnceLock<mpsc::Sender<()>> = OnceLock::new();
    BACKTRACE_SENDER.get_or_init(|| backtrace_tx);
    BACKTRACE.lock().reserve(100);

    fn handle_backtrace_signal() {
        unsafe {
            extern "C" fn handle_sigusr2(_i: c_int) {
                unsafe {
                    // ASYNC SIGNAL SAFETY: This lock is only accessed one other time,
                    // which can only be triggered by This signal handler. In addition,
                    // this signal handler is immediately removed by SA_RESETHAND, and this
                    // signal handler cannot be re-entrant due to to the SIGUSR2 mask defined
                    // below
                    let mut bt = BACKTRACE.lock();
                    bt.clear();
                    backtrace::trace_unsynchronized(|frame| {
                        if bt.len() < bt.capacity() {
                            bt.push(frame.clone());
                            true
                        } else {
                            false
                        }
                    });
                }

                BACKTRACE_SENDER.get().unwrap().send(()).ok();
            }

            let mut mask = SigSet::empty();
            mask.add(SIGUSR2);
            sigaction(
                Signal::SIGUSR2,
                &SigAction::new(
                    SigHandler::Handler(handle_sigusr2),
                    SaFlags::SA_RESTART | SaFlags::SA_RESETHAND,
                    mask,
                ),
            )
            .log_err();
        }
    }

    handle_backtrace_signal();
    let main_thread = pthread::pthread_self();

    let (mut tx, mut rx) = futures::channel::mpsc::channel(3);
    foreground_executor
        .spawn(async move { while let Some(_) = rx.next().await {} })
        .detach();

    background_executor
        .spawn({
            let background_executor = background_executor.clone();
            async move {
                loop {
                    background_executor.timer(Duration::from_secs(1)).await;
                    match tx.try_send(()) {
                        Ok(_) => continue,
                        Err(e) => {
                            if e.into_send_error().is_full() {
                                pthread::pthread_kill(main_thread, SIGUSR2).log_err();
                            }
                            // Only detect the first hang
                            break;
                        }
                    }
                }
            }
        })
        .detach();
}
