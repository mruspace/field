//! The task network lock (docs/spec.md, section 11): with `task_network =
//! deny`, a task process cannot open a socket of any family but AF_UNIX, so
//! no build of the task can send to a network, however wrong it is. A
//! seccomp filter, set in the child between fork and exec, makes `socket`
//! fail with EACCES for every other family. Linux on x86_64, aarch64 and
//! 32-bit ARM; elsewhere the lock is not enforced, like the memory limit.
//!
//! The filter is built in the parent, before the fork, so the child only
//! makes two `prctl` calls (async-signal-safe) and allocates nothing.

/// The filter program, ready to install in a child.
#[derive(Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct Filter {
    #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "arm")))]
    prog: Vec<libc::sock_filter>,
}

/// True where `task_network = deny` is enforced.
pub const ENFORCED: bool =
    cfg!(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "arm")));

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "arm")))]
mod bpf {
    pub const LD_W_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    pub const JEQ_K: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
    pub const RET_K: u16 = 0x06; // BPF_RET | BPF_K
    /// Offsets in struct seccomp_data: nr, arch, then args[0] at 16 (its low
    /// 32 bits on these little-endian targets).
    pub const NR: u32 = 0;
    pub const ARCH: u32 = 4;
    pub const ARG0: u32 = 16;

    #[cfg(target_arch = "x86_64")]
    pub const AUDIT_ARCH: u32 = 0xC000_003E;
    #[cfg(target_arch = "aarch64")]
    pub const AUDIT_ARCH: u32 = 0xC000_00B7;
    #[cfg(target_arch = "arm")]
    pub const AUDIT_ARCH: u32 = 0x4000_0028;

    /// The syscalls that open a socket on this target: `socket`, and on
    /// x86_64 its x32 number, which the kernel accepts under the same arch.
    #[cfg(target_arch = "x86_64")]
    pub const SOCKET: &[u32] = &[41, 41 | 0x4000_0000];
    #[cfg(target_arch = "aarch64")]
    pub const SOCKET: &[u32] = &[198];
    #[cfg(target_arch = "arm")]
    pub const SOCKET: &[u32] = &[281];
    /// Old multiplexed socket calls, refused outright: EABI programs do not
    /// use `socketcall`, and it would hide the family from the filter.
    #[cfg(target_arch = "arm")]
    pub const SOCKETCALL: &[u32] = &[102];
    #[cfg(not(target_arch = "arm"))]
    pub const SOCKETCALL: &[u32] = &[];
}

impl Filter {
    /// The filter for this target, or `None` where it is not enforced.
    pub fn new() -> Option<Filter> {
        if !ENFORCED {
            return None;
        }
        #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "arm")))]
        {
            use bpf::*;
            let stmt = |code, k| libc::sock_filter { code, jt: 0, jf: 0, k };
            let jeq = |k, jt, jf| libc::sock_filter { code: JEQ_K, jt, jf, k };
            let allow = libc::SECCOMP_RET_ALLOW;
            let deny = libc::SECCOMP_RET_ERRNO | libc::EACCES as u32;
            // A syscall from another arch (a 32-bit call on a 64-bit kernel)
            // is refused: the numbers below would not mean `socket` there.
            let mut p = vec![
                stmt(LD_W_ABS, ARCH),
                jeq(AUDIT_ARCH, 1, 0),
                stmt(RET_K, libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
                stmt(LD_W_ABS, NR),
            ];
            // After the number load, at positions from 0: S socketcall tests,
            // K socket tests, `ret allow` at S+K, the family check from S+K+1
            // (load, test, `ret allow`), and `ret deny` at S+K+4. A jump from
            // position p to t skips t-p-1 instructions.
            let (s, k) = (SOCKETCALL.len(), SOCKET.len());
            let (family, denied) = (s + k + 1, s + k + 4);
            for (i, nr) in SOCKETCALL.iter().enumerate() {
                p.push(jeq(*nr, (denied - i - 1) as u8, 0));
            }
            for (j, nr) in SOCKET.iter().enumerate() {
                p.push(jeq(*nr, (family - (s + j) - 1) as u8, 0));
            }
            p.push(stmt(RET_K, allow));
            p.push(stmt(LD_W_ABS, ARG0));
            p.push(jeq(libc::AF_UNIX as u32, 0, 1));
            p.push(stmt(RET_K, allow));
            p.push(stmt(RET_K, deny));
            Some(Filter { prog: p })
        }
        #[cfg(not(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "arm")
        )))]
        None
    }

    /// Install in the calling process. For the child between fork and exec:
    /// two `prctl` calls and nothing else.
    pub fn install(&self) -> std::io::Result<()> {
        #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "arm")))]
        {
            let prog = libc::sock_fprog { len: self.prog.len() as u16, filter: self.prog.as_ptr() as *mut _ };
            // SAFETY: prctl with a filter program that outlives the call.
            unsafe {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &prog as *const libc::sock_fprog) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }
}
