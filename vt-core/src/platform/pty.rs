use portable_pty::MasterPty;

#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock};

#[cfg(unix)]
use nix::sys::signal::{kill, Signal};
#[cfg(unix)]
use nix::unistd::Pid;

/// 활성 PTY platform이 제공하는 process-group leader를 반환한다.
pub fn process_group_leader(
    #[cfg(unix)] master: &dyn MasterPty,
    #[cfg(not(unix))] _master: &dyn MasterPty,
) -> Option<i32> {
    #[cfg(unix)]
    {
        master.process_group_leader()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// 마스터가 썼고 자식이 아직 읽지 않은 PTY 입력 큐의 바이트 수. 진단 측정 관측이다.
pub fn pending_input(
    #[cfg(unix)] master: &dyn MasterPty,
    #[cfg(not(unix))] _master: &dyn MasterPty,
) -> Result<usize, String> {
    #[cfg(unix)]
    {
        let fd = master.as_raw_fd().ok_or("PTY master fd is unavailable")?;
        let mut pending: libc::c_int = 0;
        // xnu(BSD 계열) PTY 는 마스터와 슬레이브가 struct tty 를 공유한다. 마스터의 쓰기는 줄 규율
        // 입력 처리로 t_rawq/t_canq 에 쌓이고 FIONREAD(ttnread) 는 그 합, 즉 마스터가 쓰고 자식이
        // 아직 읽지 않은 바이트 수를 반환한다. TIOCOUTQ 는 t_outq, 즉 자식이 쓴 출력이므로 이 값이
        // 아니다(소유 검사에서 둘 다 측정해 구별했다). canonical 모드에서는 완성 전 줄이 t_rawq 의
        // 조립 버퍼에 남아 세지 않는다.
        let result = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut pending) };
        if result < 0 {
            return Err(format!(
                "read PTY pending input: {}",
                std::io::Error::last_os_error()
            ));
        }
        usize::try_from(pending).map_err(|_| format!("PTY pending input {pending} is negative"))
    }
    #[cfg(not(unix))]
    {
        Err("PTY pending input is not implemented on this platform".to_string())
    }
}

/// platform이 PTY process group을 노출하면 그 group을 종료한다.
pub fn kill_process_group(
    #[cfg(unix)] group: Option<i32>,
    #[cfg(not(unix))] _group: Option<i32>,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let group = group.ok_or("PTY process group is unavailable")?;
        match kill(Pid::from_raw(-group), Signal::SIGKILL) {
            Ok(()) => Ok(()),
            Err(nix::errno::Errno::ESRCH) => Ok(()),
            #[cfg(target_os = "macos")]
            Err(nix::errno::Errno::EPERM) => {
                // macOS 는 구성원이 모두 exit() 을 처리하는 중이거나 회수를 기다리는(좀비) 그룹의 신호에
                // EPERM 을 돌려준다. 셸이 읽히지 않은 출력을 남기고 끝나면 터미널을 닫으며 출력이 비워지기를
                // 기다리는 동안 종료 중이다. 그런 그룹은 끝낼 프로세스가 없다. 끝나지 않은 구성원이 있으면
                // 그 상태를 적어 실패한다.
                let members = crate::platform::darwin::process_group::members(group)
                    .map_err(|error| format!("kill PTY process group {group}: EPERM; {error}"))?;
                if members.iter().all(|member| member.ended()) {
                    return Ok(());
                }
                Err(format!(
                    "kill PTY process group {group}: EPERM with members {members:?}"
                ))
            }
            Err(error) => Err(format!("kill PTY process group {group}: {error}")),
        }
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

/// Rust test process 내부와 process 사이에서 실제 PTY test를 직렬화한다.
#[cfg(test)]
pub(crate) fn native_pty_test_lock() -> NativePtyTestLock {
    static LOCAL: OnceLock<Mutex<()>> = OnceLock::new();
    let local = LOCAL
        .get_or_init(|| Mutex::new(()))
        .lock()
        // 기본값: 검사 전용 잠금이다. 앞선 검사가 잠금을 쥔 채 실패해도 그 실패는 이미 보고되었고 뒤 검사는 계속 돈다.
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let path = std::env::temp_dir().join("soksak-vt-core-pty-tests.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)
            .expect("PTY test lock file must open");
        nix::fcntl::flock(file.as_raw_fd(), nix::fcntl::FlockArg::LockExclusive)
            .expect("PTY test lock must acquire");
        NativePtyTestLock {
            _local: local,
            _file: file,
        }
    }
    #[cfg(not(unix))]
    {
        NativePtyTestLock { _local: local }
    }
}

#[cfg(test)]
pub(crate) struct NativePtyTestLock {
    _local: MutexGuard<'static, ()>,
    #[cfg(unix)]
    _file: std::fs::File,
}
