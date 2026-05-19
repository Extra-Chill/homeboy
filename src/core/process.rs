pub fn pid_is_running(pid: u32) -> bool {
    if pid > i32::MAX as u32 {
        return false;
    }

    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
    }

    #[cfg(not(unix))]
    {
        pid == std::process::id()
    }
}
