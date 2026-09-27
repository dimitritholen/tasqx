//! Running an external executable that answers one request with one reply.
//!
//! The layer under [`crate::remote`] (D198), kept free of anything
//! connector-specific because DESIGN.md §6b's hooks have the same shape: one
//! JSON object on stdin, one on stdout, stderr for a person, and a kill deadline
//! so a hung child cannot wedge the command that started it.
//!
//! Blocking `std::process` and threads, no async runtime. Three bounds hold
//! whatever the child does:
//!  * **time** — past [`Limits::timeout`] the child is killed and reaped;
//!  * **stdout** — past [`Limits::max_stdout`] bytes it is killed too, because
//!    the reply is a small JSON document and anything that size is a bug (a
//!    connector hands a blob over by FILE, never through the pipe);
//!  * **stderr** — only the last [`STDERR_KEPT`] bytes are held; it is read to
//!    the end so the child never blocks on a full pipe.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// How much stderr is held in memory: its last this-many bytes.
pub const STDERR_KEPT: usize = 64 * 1024;

/// How much of stderr an error message quotes: its END, where the reason
/// usually is.
pub const STDERR_QUOTED: usize = 1024;

/// The bounds a child runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Wall-clock time from spawn to exit before the child is killed.
    pub timeout: Duration,
    /// The most stdout accepted; one byte more and the child is killed.
    pub max_stdout: usize,
}

impl Default for Limits {
    /// 120 seconds, which leaves a connector room for a slow upload of a large
    /// store, and 1 MiB of stdout, which no well-formed reply comes near.
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            max_stdout: 1024 * 1024,
        }
    }
}

/// What a child that exited on its own produced.
#[derive(Debug)]
pub struct Output {
    /// How it exited.
    pub status: ExitStatus,
    /// Everything it wrote to stdout (at most [`Limits::max_stdout`] bytes).
    pub stdout: Vec<u8>,
    /// The last [`STDERR_KEPT`] bytes of its stderr, lossily decoded.
    pub stderr: String,
}

/// Why a child produced no [`Output`].
#[derive(Debug)]
pub enum ExecError {
    /// It could not be started.
    Spawn {
        /// The executable.
        program: PathBuf,
        /// The operating system's reason.
        source: std::io::Error,
    },
    /// It ran past [`Limits::timeout`] and was killed.
    Timeout {
        /// The executable.
        program: PathBuf,
        /// The limit it ran past.
        after: Duration,
        /// Its stderr up to the kill.
        stderr: String,
    },
    /// It wrote more than [`Limits::max_stdout`] bytes and was killed.
    StdoutTooLarge {
        /// The executable.
        program: PathBuf,
        /// The limit it ran past.
        limit: usize,
    },
    /// Waiting on it failed (not the child's fault).
    Wait {
        /// The executable.
        program: PathBuf,
        /// The operating system's reason.
        source: std::io::Error,
    },
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn { program, source } => {
                write!(f, "cannot run {}: {source}", program.display())
            }
            Self::Timeout {
                program,
                after,
                stderr,
            } => {
                write!(
                    f,
                    "{} did not answer within {}s and was stopped",
                    program.display(),
                    after.as_secs_f64()
                )?;
                write_stderr(f, stderr)
            }
            Self::StdoutTooLarge { program, limit } => write!(
                f,
                "{} wrote more than {limit} bytes to stdout and was stopped; \
                 a reply is one small JSON object",
                program.display()
            ),
            Self::Wait { program, source } => {
                write!(f, "lost track of {}: {source}", program.display())
            }
        }
    }
}

impl std::error::Error for ExecError {}

/// Append `; stderr: <the last STDERR_QUOTED bytes>` when there is any.
pub(crate) fn write_stderr(f: &mut std::fmt::Formatter<'_>, stderr: &str) -> std::fmt::Result {
    let quoted = stderr_excerpt(stderr);
    if quoted.is_empty() {
        Ok(())
    } else {
        write!(f, "; stderr: {quoted}")
    }
}

/// The end of `stderr`, trimmed, at most [`STDERR_QUOTED`] bytes, cut on a
/// character boundary and led by `…` when something was cut.
pub fn stderr_excerpt(stderr: &str) -> String {
    let s = stderr.trim();
    if s.len() <= STDERR_QUOTED {
        return s.to_string();
    }
    let mut start = s.len() - STDERR_QUOTED;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &s[start..])
}

/// Read `r` to its end, keeping only the last `keep` bytes, lossily decoded.
/// The tail and not the head: a failing program's last line is its reason.
fn read_tail(r: &mut impl Read, keep: usize) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                // Trim in batches so a chatty child costs amortised O(1).
                if buf.len() > 2 * keep {
                    buf.drain(..buf.len() - keep);
                }
            }
        }
    }
    if buf.len() > keep {
        buf.drain(..buf.len() - keep);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// How often a running child is polled. Short enough that nobody notices it on
/// a fast reply, long enough to cost nothing while a slow one uploads.
const POLL: Duration = Duration::from_millis(5);

/// After the child exits, how long its pipes may stay open (a grandchild that
/// inherited them) before the reply is given up on.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Run `cmd` with `stdin` as its whole input, under `limits`.
///
/// Stdin, stdout and stderr are replaced with pipes; everything else about
/// `cmd` (arguments, environment, working directory) is the caller's. The exit
/// status is returned, not judged: what a non-zero code means is the caller's
/// protocol.
pub fn run(cmd: &mut Command, stdin: &[u8], limits: &Limits) -> Result<Output, ExecError> {
    let program = PathBuf::from(cmd.get_program());
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| ExecError::Spawn {
            program: program.clone(),
            source,
        })?;
    let started = Instant::now();

    // Stdin on its own thread: a child that answers before reading all of it
    // (or never reads it) must not deadlock us. A broken pipe is the child's
    // business and shows up in its exit status or reply.
    let mut child_in = child.stdin.take().expect("stdin was piped");
    let input = stdin.to_vec();
    std::thread::spawn(move || {
        let _ = child_in.write_all(&input);
    });

    let overflow = Arc::new(AtomicBool::new(false));
    let (out_tx, out_rx) = mpsc::channel();
    let mut child_out = child.stdout.take().expect("stdout was piped");
    let max = limits.max_stdout;
    let flag = Arc::clone(&overflow);
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        // One byte past the cap is enough to know it was crossed.
        let r = (&mut child_out).take(max as u64 + 1).read_to_end(&mut buf);
        if buf.len() > max {
            flag.store(true, Ordering::SeqCst);
        }
        let _ = out_tx.send(r.map(|_| buf));
    });

    let (err_tx, err_rx) = mpsc::channel();
    let mut child_err = child.stderr.take().expect("stderr was piped");
    std::thread::spawn(move || {
        let _ = err_tx.send(read_tail(&mut child_err, STDERR_KEPT));
    });

    let kill = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    let status = loop {
        if overflow.load(Ordering::SeqCst) {
            kill(&mut child);
            return Err(ExecError::StdoutTooLarge {
                program,
                limit: max,
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(source) => {
                kill(&mut child);
                return Err(ExecError::Wait { program, source });
            }
        }
        if started.elapsed() >= limits.timeout {
            kill(&mut child);
            let stderr = err_rx.recv_timeout(DRAIN_GRACE).unwrap_or_default();
            return Err(ExecError::Timeout {
                program,
                after: limits.timeout,
                stderr,
            });
        }
        std::thread::sleep(POLL);
    };

    let stdout = match out_rx.recv_timeout(DRAIN_GRACE) {
        Ok(Ok(buf)) if buf.len() > max => {
            return Err(ExecError::StdoutTooLarge {
                program,
                limit: max,
            })
        }
        Ok(Ok(buf)) => buf,
        Ok(Err(source)) => return Err(ExecError::Wait { program, source }),
        // Exited, but something it started still holds the pipe open.
        Err(_) => {
            return Err(ExecError::Wait {
                program,
                source: std::io::Error::other("it exited but its stdout stayed open"),
            })
        }
    };
    let stderr = err_rx.recv_timeout(DRAIN_GRACE).unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_keeps_its_tail_past_the_cap() {
        let input = format!("{}END", "x".repeat(10_000));
        let tail = read_tail(&mut input.as_bytes(), 100);
        assert_eq!(tail.len(), 100);
        assert!(tail.ends_with("END"));
    }

    #[test]
    fn a_short_stderr_is_quoted_whole_and_trimmed() {
        assert_eq!(stderr_excerpt("  oops\n"), "oops");
        assert_eq!(stderr_excerpt(""), "");
    }

    #[test]
    fn a_long_stderr_keeps_its_end_on_a_char_boundary() {
        let s = format!("{}{}", "é".repeat(STDERR_QUOTED), "END");
        let q = stderr_excerpt(&s);
        assert!(q.starts_with('…') && q.ends_with("END"), "{q}");
        assert!(q.len() <= STDERR_QUOTED + '…'.len_utf8() + 1);
    }
}
