//! 프로세스 그룹 구성원과 그 상태를 읽는다.

/// 프로세스 그룹의 구성원 하나.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    pub pid: i32,
    /// 끝났고 아직 회수되지 않은 프로세스.
    pub zombie: bool,
    /// exit() 을 처리하는 중인 프로세스. 예를 들어 읽히지 않은 출력이 남은 터미널을 닫으며 기다린다.
    pub exiting: bool,
}

impl Member {
    /// 더 끝낼 것이 없는 구성원. macOS 는 이런 구성원만 남은 그룹의 신호에 EPERM 으로 답한다.
    pub fn ended(&self) -> bool {
        self.zombie || self.exiting
    }
}

// sys/proc_info.h 의 PROC_FLAG_INEXIT. libc 크레이트는 이 값을 내보내지 않는다.
const PROC_FLAG_INEXIT: u32 = 4;

// sys/proc_info.h 의 PROC_PGRP_ONLY. libc 크레이트는 이 값을 내보내지 않는다.
const PROC_PGRP_ONLY: u32 = 2;

/// 프로세스 그룹의 구성원과 상태를 읽는다. 읽는 동안 회수된 프로세스는 결과에 없다.
pub fn members(group: i32) -> Result<Vec<Member>, String> {
    let size =
        unsafe { libc::proc_listpids(PROC_PGRP_ONLY, group as u32, std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(format!(
            "list process group {group}: {}",
            std::io::Error::last_os_error()
        ));
    }
    // 첫 호출과 둘째 호출 사이에 늘어난 구성원을 담도록 여유를 둔다.
    let capacity = size as usize / std::mem::size_of::<i32>() + 16;
    let mut pids = vec![0i32; capacity];
    let filled = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            group as u32,
            pids.as_mut_ptr().cast(),
            (capacity * std::mem::size_of::<i32>()) as libc::c_int,
        )
    };
    if filled < 0 {
        return Err(format!(
            "list process group {group}: {}",
            std::io::Error::last_os_error()
        ));
    }
    pids.truncate(filled as usize / std::mem::size_of::<i32>());
    let mut members = Vec::new();
    for pid in pids.into_iter().filter(|pid| *pid > 0) {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // PROC_PIDTBSDINFO 는 arg 가 0 이면 좀비에 ESRCH 로 답하고, 1 이면 좀비도 찾는다.
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                1,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if read == size {
            members.push(Member {
                pid,
                zombie: info.pbi_status == libc::SZOMB,
                exiting: info.pbi_flags & PROC_FLAG_INEXIT != 0,
            });
            continue;
        }
        // 목록을 읽은 뒤 회수된 프로세스는 정보를 주지 않는다. 그 밖의 실패는 상태를 모르는 것이다.
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(format!("read process {pid} of group {group}: {error}"));
        }
    }
    Ok(members)
}
