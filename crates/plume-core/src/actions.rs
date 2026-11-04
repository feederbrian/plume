//! Process control: signals and priority, race-free against PID reuse.
//!
//! A PID can be recycled between the moment the user clicks "End" and the
//! moment we call kill(). We open a pidfd first (which pins the process), then
//! check its start time against what the UI showed, then signal through the
//! pidfd. If the start time differs, the original process is gone and we
//! refuse rather than signal a stranger.

use crate::proc::{start_ticks, thread_ids};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sig {
    Term,
    Kill,
    Stop,
    Cont,
    Hup,
    Int,
}

impl Sig {
    pub fn raw(self) -> libc::c_int {
        match self {
            Sig::Term => libc::SIGTERM,
            Sig::Kill => libc::SIGKILL,
            Sig::Stop => libc::SIGSTOP,
            Sig::Cont => libc::SIGCONT,
            Sig::Hup => libc::SIGHUP,
            Sig::Int => libc::SIGINT,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Sig::Term => "TERM",
            Sig::Kill => "KILL",
            Sig::Stop => "STOP",
            Sig::Cont => "CONT",
            Sig::Hup => "HUP",
            Sig::Int => "INT",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionError {
    /// The process exited (or its PID now belongs to someone else).
    Gone,
    /// Needs elevated privileges; the UI can offer `privileged_*` via pkexec.
    PermissionDenied,
    Refused(&'static str),
    Os(i32),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::Gone => write!(f, "The process no longer exists"),
            ActionError::PermissionDenied => write!(f, "Permission denied"),
            ActionError::Refused(why) => write!(f, "{why}"),
            ActionError::Os(e) => write!(f, "{}", std::io::Error::from_raw_os_error(*e)),
        }
    }
}

fn guard(pid: i32, sig: Option<Sig>) -> Result<(), ActionError> {
    if pid <= 1 {
        return Err(ActionError::Refused("Refusing to act on PID 0/1"));
    }
    if pid == std::process::id() as i32 && sig == Some(Sig::Stop) {
        return Err(ActionError::Refused("Stopping Plume itself would freeze it"));
    }
    Ok(())
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Sends `sig` to the process identified by (`pid`, `start`).
pub fn signal(pid: i32, start: u64, sig: Sig) -> Result<(), ActionError> {
    guard(pid, Some(sig))?;
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as libc::c_int;
    if pidfd < 0 {
        return match errno() {
            libc::ESRCH => Err(ActionError::Gone),
            // Very old kernel: fall back to check-then-kill.
            libc::ENOSYS => {
                if start_ticks(pid) != Some(start) {
                    return Err(ActionError::Gone);
                }
                map(unsafe { libc::kill(pid, sig.raw()) })
            }
            e => Err(ActionError::Os(e)),
        };
    }
    let result = if start_ticks(pid) != Some(start) {
        Err(ActionError::Gone)
    } else {
        map(unsafe {
            libc::syscall(libc::SYS_pidfd_send_signal, pidfd, sig.raw(), std::ptr::null::<libc::c_void>(), 0) as libc::c_int
        })
    };
    unsafe { libc::close(pidfd) };
    result
}

fn map(r: libc::c_int) -> Result<(), ActionError> {
    if r == 0 {
        return Ok(());
    }
    match errno() {
        libc::EPERM | libc::EACCES => Err(ActionError::PermissionDenied),
        libc::ESRCH => Err(ActionError::Gone),
        e => Err(ActionError::Os(e)),
    }
}

/// Sets the nice value of every thread of the process (Linux applies
/// setpriority(PRIO_PROCESS) to a single thread).
pub fn set_nice(pid: i32, start: u64, nice: i32) -> Result<(), ActionError> {
    guard(pid, None)?;
    if start_ticks(pid) != Some(start) {
        return Err(ActionError::Gone);
    }
    let nice = nice.clamp(-20, 19);
    let mut tids = thread_ids(pid);
    if tids.is_empty() {
        tids.push(pid);
    }
    for tid in tids {
        let r = unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, nice) };
        if r != 0 {
            let e = errno();
            if e != libc::ESRCH {
                return map(r);
            }
        }
    }
    Ok(())
}

/// argv for sending a signal as root through polkit (`pkexec`).
pub fn privileged_signal_argv(pid: i32, sig: Sig) -> Vec<String> {
    vec!["pkexec".into(), "/usr/bin/kill".into(), "-s".into(), sig.name().into(), pid.to_string()]
}

/// argv for renicing all threads as root through polkit.
pub fn privileged_nice_argv(pid: i32, nice: i32) -> Vec<String> {
    let mut v = vec!["pkexec".into(), "/usr/bin/renice".into(), "-n".into(), nice.clamp(-20, 19).to_string(), "-p".into()];
    let tids = thread_ids(pid);
    if tids.is_empty() {
        v.push(pid.to_string());
    } else {
        v.extend(tids.iter().map(|t| t.to_string()));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn refuses_pid1_and_detects_reuse() {
        assert!(matches!(signal(1, 0, Sig::Term), Err(ActionError::Refused(_))));
        assert!(matches!(signal(0, 0, Sig::Term), Err(ActionError::Refused(_))));
        assert!(matches!(signal(-1, 0, Sig::Kill), Err(ActionError::Refused(_))));
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id() as i32;
        let start = start_ticks(pid).unwrap();
        // Wrong start time → treated as a different process.
        assert_eq!(signal(pid, start + 1, Sig::Term), Err(ActionError::Gone));
        assert!(child.try_wait().unwrap().is_none(), "child must survive");
        // Stop / continue / renice / terminate the real one.
        signal(pid, start, Sig::Stop).unwrap();
        signal(pid, start, Sig::Cont).unwrap();
        set_nice(pid, start, 5).unwrap();
        signal(pid, start, Sig::Term).unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
    }
}
