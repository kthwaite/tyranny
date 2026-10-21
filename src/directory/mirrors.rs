use anyhow::{Context, Result, ensure};
use std::{
    ffi::CStr,
    net::{SocketAddr, ToSocketAddrs},
};

const SUFFIX: &str = ".api.radio-browser.info";
const LOOKUP_NAME: &str = "all.api.radio-browser.info";
const NAME_BUFFER: usize = 1025;

/// Keeps radio-browser names, sorted and deduped, then rotated by `offset`.
pub(super) fn select_hosts(names: impl IntoIterator<Item = String>, offset: usize) -> Vec<String> {
    let mut hosts: Vec<String> = names
        .into_iter()
        .map(|name| name.trim_end_matches('.').to_ascii_lowercase())
        .filter(|name| {
            name.len() > SUFFIX.len()
                && name.ends_with(SUFFIX)
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
                && !name.starts_with(['.', '-'])
                && !name.contains("..")
        })
        .collect();
    hosts.sort();
    hosts.dedup();
    if !hosts.is_empty() {
        let shift = offset % hosts.len();
        hosts.rotate_left(shift);
    }
    hosts
}

/// Finds mirrors by DNS: forward lookup of the pool name, then a reverse lookup of each address.
pub(super) fn resolve() -> Result<Vec<String>> {
    let addresses = (LOOKUP_NAME, 443)
        .to_socket_addrs()
        .context("looking up radio-browser.info mirrors")?;
    let names = addresses.filter_map(|address| reverse(&address));
    let offset = uuid::Uuid::new_v4().as_u128() as usize;
    let hosts = select_hosts(names, offset);
    ensure!(!hosts.is_empty(), "no radio-browser.info mirrors found");
    Ok(hosts)
}

fn reverse(address: &SocketAddr) -> Option<String> {
    let mut host = [0 as libc::c_char; NAME_BUFFER];
    // SAFETY: each sockaddr is fully initialised and passed with its own size; `host` is
    // writable for NAME_BUFFER bytes and getnameinfo NUL-terminates it on success.
    let status = unsafe {
        match address {
            SocketAddr::V4(v4) => {
                let mut raw: libc::sockaddr_in = std::mem::zeroed();
                raw.sin_family = libc::AF_INET as libc::sa_family_t;
                raw.sin_addr.s_addr = u32::from_ne_bytes(v4.ip().octets());
                #[cfg(target_vendor = "apple")]
                {
                    raw.sin_len = size_of::<libc::sockaddr_in>() as u8;
                }
                getnameinfo(
                    std::ptr::from_ref(&raw).cast(),
                    size_of_val(&raw),
                    &mut host,
                )
            }
            SocketAddr::V6(v6) => {
                let mut raw: libc::sockaddr_in6 = std::mem::zeroed();
                raw.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                raw.sin6_addr.s6_addr = v6.ip().octets();
                #[cfg(target_vendor = "apple")]
                {
                    raw.sin6_len = size_of::<libc::sockaddr_in6>() as u8;
                }
                getnameinfo(
                    std::ptr::from_ref(&raw).cast(),
                    size_of_val(&raw),
                    &mut host,
                )
            }
        }
    };
    if status != 0 {
        return None;
    }
    // SAFETY: getnameinfo succeeded, so `host` holds a NUL-terminated string.
    let name = unsafe { CStr::from_ptr(host.as_ptr()) };
    name.to_str().ok().map(str::to_owned)
}

unsafe fn getnameinfo(
    address: *const libc::sockaddr,
    length: usize,
    host: &mut [libc::c_char; NAME_BUFFER],
) -> libc::c_int {
    unsafe {
        libc::getnameinfo(
            address,
            length as libc::socklen_t,
            host.as_mut_ptr(),
            NAME_BUFFER as libc::socklen_t,
            std::ptr::null_mut(),
            0,
            libc::NI_NAMEREQD,
        )
    }
}
