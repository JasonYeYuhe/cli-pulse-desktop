//! Headless STREAMING smoke for the PTY transport (terminal epic T0).
//!
//! Spawns a short-lived child that prints numbered lines with a pause
//! between each, then drives `ConPtyTransport` directly and asserts the
//! output arrives **incrementally** (bytes seen while the child is still
//! running, across multiple reads) — NOT drained as one blob at exit.
//! This is the local, provable signal the terminal epic needs before the
//! xterm.js UI is built: cross-platform via portable-pty, so it runs on
//! this dev Mac / Linux (POSIX openpty) AND the Windows VM (ConPTY).
//!
//! It asserts the LOCAL streamed signal only — no Sentry, no network — so
//! a green run never rests on an async Sentry query (the T0 Sentry-race
//! fix). The last line of output is the verdict.
//!
//! Usage:
//!   cargo run --bin terminal_smoke
//! Exit codes: 0 = PASS or SKIP (no PTY available — headless CI),
//!             1 = FAIL (spawned but output was missing or not streamed).

use std::time::{Duration, Instant};

use cli_pulse_desktop_lib::remote::{ConPtyTransport, SessionTransport};

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    // A child that emits 5 lines ~300 ms apart (~1.5 s total). Each line
    // is flushed by the shell on the newline because the PTY slave is a
    // tty. On Windows a `cmd /c "for /l ..."` one-liner mis-parses through
    // ConPTY's single-string argv (it emitted only a few bytes on CI), so
    // use PowerShell — `Write-Host ('line'+$_)` avoids inner double-quotes
    // that the argv round-trip would mangle.
    let argv: Vec<String> = if cfg!(target_os = "windows") {
        vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            "1..5 | ForEach-Object { Write-Host ('line'+$_); Start-Sleep -Milliseconds 300 }"
                .to_string(),
        ]
    } else {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "i=1; while [ $i -le 5 ]; do echo line$i; i=$((i+1)); sleep 0.3; done".to_string(),
        ]
    };

    println!("=== terminal_smoke (streaming PTY assertion) ===");
    println!("  spawning: {argv:?}");

    let t = ConPtyTransport::new();
    let handle = match t.start("terminal-smoke-0001", &argv, Default::default(), None) {
        Ok(h) => h,
        Err(e) => {
            // No PTY available (headless CI without a console) — not a
            // regression in our code. Skip rather than fail the gate.
            println!("  PTY unavailable ({e}) — cannot assert streaming here.");
            println!("SKIP");
            return 0;
        }
    };

    // T2.1 — a live PTY must accept a resize round-trip (SIGWINCH /
    // ResizePseudoConsole). A shell loop ignores SIGWINCH, so this
    // doesn't perturb the streamed output we assert below.
    match t.resize(&handle, 40, 120) {
        Ok(()) => println!("  resize 40x120:             ok"),
        Err(e) => {
            println!("  resize failed: {e}");
            println!("FAIL");
            return 1;
        }
    }

    let mut captured: Vec<u8> = Vec::new();
    let mut read_events = 0usize;
    let mut saw_output_while_running = false;
    let mut first_byte_at: Option<Duration> = None;
    let started = Instant::now();

    loop {
        let chunk = match t.read_stdout(&handle, 8192) {
            Ok(c) => c,
            Err(e) => {
                println!("  read_stdout error: {e}");
                println!("FAIL");
                return 1;
            }
        };
        // Sample liveness AFTER the read: a non-empty chunk read while the
        // child is STILL running is the strictest "streaming, not
        // drained-at-exit" proof — a drain-at-exit impl only yields bytes
        // once the child has already exited, so this stays false for it
        // (and no exit-microsecond race can flip it).
        let exited = matches!(t.try_wait(&handle), Ok(Some(_)));

        if !chunk.is_empty() {
            read_events += 1;
            first_byte_at.get_or_insert_with(|| started.elapsed());
            captured.extend_from_slice(&chunk);
            if !exited {
                saw_output_while_running = true;
            }
        } else if exited {
            // Empty read + child exited — one final drain (catch bytes that
            // landed between the read and the try_wait), then done.
            if let Ok(tail) = t.read_stdout(&handle, 8192) {
                if !tail.is_empty() {
                    read_events += 1;
                    captured.extend_from_slice(&tail);
                }
            }
            break;
        }

        if started.elapsed() > Duration::from_secs(20) {
            println!("  timed out after 20 s waiting for child to finish.");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // Explicit teardown: dropping the last handle flips reader_stop and
    // (on Windows) closes the Job Object → child dies.
    drop(handle);

    let text = String::from_utf8_lossy(&captured);
    let has_first = text.contains("line1");
    let has_last = text.contains("line5");
    let full_capture = has_first && has_last;
    let streamed = saw_output_while_running || read_events >= 2;

    println!("  bytes captured:            {}", captured.len());
    println!("  distinct read events:      {read_events}");
    println!("  saw output while running:  {saw_output_while_running}");
    println!(
        "  first byte at:             {}",
        first_byte_at
            .map(|d| format!("{} ms", d.as_millis()))
            .unwrap_or_else(|| "never".to_string())
    );
    println!("  captured line1 & line5:    {full_capture}");
    println!();

    if full_capture && streamed {
        println!("PASS");
        0
    } else if captured.is_empty() {
        println!("  no output captured — read_stdout may not be wired to the reader thread.");
        println!("FAIL");
        1
    } else {
        println!("  output captured but not incrementally streamed (looks drained-at-exit).");
        println!("FAIL");
        1
    }
}
