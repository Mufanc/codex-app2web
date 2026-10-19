use std::io;
use std::mem;
use std::ptr;

use anyhow::{ensure, Context};

pub(super) fn arguments(pid: libc::pid_t) -> anyhow::Result<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = 0;

    // KERN_PROCARGS2 returns argc, the executable path, padding, then NUL-separated argv.
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as _,
            ptr::null_mut(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };

    if result != 0 {
        return Err(io::Error::last_os_error().into());
    }

    let mut bytes = vec![0_u8; size];
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as _,
            bytes.as_mut_ptr().cast(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };

    if result != 0 {
        return Err(io::Error::last_os_error().into());
    }

    bytes.truncate(size);

    parse_arguments(&bytes)
}

fn parse_arguments(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    let argc = bytes.get(..mem::size_of::<i32>()).context("Missing process argument count")?;
    let argc = i32::from_ne_bytes(argc.try_into()?);

    ensure!(argc > 0, "Invalid process argument count");

    let bytes = &bytes[mem::size_of::<i32>()..];
    let executable_end = bytes.iter().position(|byte| *byte == 0).context("Missing executable path terminator")?;
    let bytes = &bytes[executable_end + 1..];
    let start = bytes.iter().position(|byte| *byte != 0).context("Missing process arguments")?;
    let mut remaining = &bytes[start..];
    let mut args = Vec::new();

    for _ in 0..argc {
        let end = remaining.iter().position(|byte| *byte == 0).context("Truncated process arguments")?;

        args.push(String::from_utf8_lossy(&remaining[..end]).into_owned());
        remaining = &remaining[end + 1..];
    }

    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_current_process_arguments() {
        let actual = arguments(std::process::id() as libc::pid_t).unwrap();
        assert_eq!(actual, std::env::args().collect::<Vec<_>>());
    }

    #[test]
    fn preserves_argument_boundaries_and_excludes_environment() {
        let mut bytes = 3_i32.to_ne_bytes().to_vec();
        bytes
            .extend_from_slice(b"/Apps/Codex App\0\0\0/Apps/Codex App\0\0--remote-debugging-port=9341\0SECRET=value\0");
        assert_eq!(
            parse_arguments(&bytes).unwrap(),
            ["/Apps/Codex App", "", "--remote-debugging-port=9341"]
        );
        assert!(parse_arguments(&bytes[..10]).is_err());
        assert!(parse_arguments(&[]).is_err());
    }
}
