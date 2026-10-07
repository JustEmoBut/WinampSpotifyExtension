//! Album art through Winamp's Wasabi service API: registers an `svc_albumArtProvider`
//! (Agave/AlbumArt/svc_albumArtProvider.h) that serves Spotify covers and YouTube thumbnails.
//! Wasabi objects are C++ `Dispatchable`s: a vtable with a single thiscall `_dispatch(msg,
//! retval, params, nparam)`; each param is passed as a pointer to its value (bfc/dispatch.h).

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::{SendMessageW, WM_WA_IPC, cover_url, module, to_wide, wide_to_string};

const IPC_GET_API_SERVICE: isize = 3025;
/// Returned by IPC_GET_API_SERVICE when the Winamp version has no service API.
const NO_API_SERVICE: usize = 1;

// api_service.h
const API_SERVICE_REGISTER: i32 = 10;
const API_SERVICE_DEREGISTER: i32 = 20;
const API_SERVICE_GETSERVICEBYGUID: i32 = 50;
// waservicefactory.h
const FACTORY_GETSERVICETYPE: i32 = 100;
const FACTORY_GETSERVICENAME: i32 = 200;
const FACTORY_GETGUID: i32 = 210;
const FACTORY_GETINTERFACE: i32 = 300;
const FACTORY_SUPPORTNONLOCKING: i32 = 301;
const FACTORY_RELEASEINTERFACE: i32 = 310;
const FACTORY_GETTESTSTRING: i32 = 500;
const FACTORY_SERVICENOTIFY: i32 = 600;
// api_memmgr.h
const MEMMGR_SYSMALLOC: i32 = 0;
// api_syscb.h, callbacks/syscb.h, callbacks/metacb.h
const SYSCB_ISSUECALLBACK: i32 = 30;
/// MK4CC('m','e','t','a')
const SYSCALLBACK_META: i32 = i32::from_be_bytes(*b"meta");
const METADATA_ART_UPDATED: i32 = 20;
// svc_albumArtProvider.h
const PROVIDER_PROVIDERTYPE: i32 = 0;
const PROVIDER_GETALBUMARTDATA: i32 = 10;
const PROVIDER_ISMINE: i32 = 20;
const PROVIDER_SETALBUMARTDATA: i32 = 30;
const PROVIDER_DELETEALBUMART: i32 = 40;
const ALBUMARTPROVIDER_SUCCESS: i32 = 0;
const ALBUMARTPROVIDER_FAILURE: i32 = 1;
const ALBUMARTPROVIDER_READONLY: i32 = 2;
const ALBUMARTPROVIDER_TYPE_EMBEDDED: i32 = 0;
/// MK3CC('a','a','p')
const SERVICE_TYPE: u32 = (b'a' as u32) << 16 | (b'a' as u32) << 8 | b'p' as u32;
/// Winamp picks an image loader by this "extension"; both CDNs serve JPEG.
const MIME_TYPE: &str = "jpg";
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const MEMMGR_GUID: Guid = Guid { data1: 0x000c_f46e, data2: 0x4df6, data3: 0x4a43, data4: [0xbb, 0xe7, 0x40, 0xe7, 0xa3, 0xea, 0x02, 0xed] };
const SYSCB_GUID: Guid = Guid { data1: 0x57b7_a1b6, data2: 0x700e, data3: 0x44ff, data4: [0x9c, 0xb0, 0x70, 0xb9, 0x2b, 0xaf, 0x39, 0x59] };
/// Identifies this provider to Winamp; generated once, never change it.
const PROVIDER_GUID: Guid = Guid { data1: 0x9f81_1bec, data2: 0x62dd, data3: 0x4dd9, data4: [0xb8, 0xdb, 0x48, 0x7c, 0x2f, 0x63, 0xf1, 0xf7] };

type DispatchFn = unsafe extern "thiscall" fn(*mut Dispatchable, i32, *mut c_void, *mut *mut c_void, i32) -> i32;

#[repr(C)]
struct Dispatchable {
    vtable: &'static DispatchFn,
}

static FACTORY: Dispatchable = Dispatchable { vtable: &(factory_dispatch as DispatchFn) };
static PROVIDER: Dispatchable = Dispatchable { vtable: &(provider_dispatch as DispatchFn) };
static SERVICE: AtomicPtr<Dispatchable> = AtomicPtr::new(std::ptr::null_mut());
static MEMMGR: AtomicPtr<Dispatchable> = AtomicPtr::new(std::ptr::null_mut());
static SYSCB: AtomicPtr<Dispatchable> = AtomicPtr::new(std::ptr::null_mut());
/// Last downloaded image; Winamp asks again for the same track (resize, skin redraw).
// ponytail: single entry; a small LRU if the Media Library browses many covers.
static LAST_IMAGE: Mutex<Option<(String, Vec<u8>)>> = Mutex::new(None);

fn ptr(d: &'static Dispatchable) -> *mut Dispatchable {
    (d as *const Dispatchable).cast_mut()
}

/// Calls `obj->_dispatch(msg, retval, params)`; true if the object handled `msg`.
unsafe fn call(obj: *mut Dispatchable, msg: i32, retval: *mut c_void, params: &mut [*mut c_void]) -> bool {
    unsafe { ((*obj).vtable)(obj, msg, retval, params.as_mut_ptr(), params.len() as i32) != 0 }
}

pub fn register() {
    let svc = unsafe { SendMessageW(module().h_main_window, WM_WA_IPC, 0, IPC_GET_API_SERVICE) } as *mut Dispatchable;
    if svc.is_null() || svc as usize == NO_API_SERVICE {
        return;
    }
    SERVICE.store(svc, Ordering::SeqCst);
    service_call(API_SERVICE_REGISTER);
}

pub fn deregister() {
    service_call(API_SERVICE_DEREGISTER);
    SERVICE.store(std::ptr::null_mut(), Ordering::SeqCst);
}

fn service_call(msg: i32) {
    let svc = SERVICE.load(Ordering::SeqCst);
    if svc.is_null() {
        return;
    }
    let mut factory = ptr(&FACTORY);
    let mut ret = 0i32;
    unsafe { call(svc, msg, (&raw mut ret).cast(), &mut [(&raw mut factory).cast()]) };
}

/// A Wasabi API looked up by GUID once and cached in `cache`.
fn api(guid: Guid, cache: &AtomicPtr<Dispatchable>) -> *mut Dispatchable {
    let cached = cache.load(Ordering::SeqCst);
    let svc = SERVICE.load(Ordering::SeqCst);
    if !cached.is_null() || svc.is_null() {
        return cached;
    }
    let mut guid = guid;
    let mut factory: *mut Dispatchable = std::ptr::null_mut();
    unsafe { call(svc, API_SERVICE_GETSERVICEBYGUID, (&raw mut factory).cast(), &mut [(&raw mut guid).cast()]) };
    if factory.is_null() {
        return factory;
    }
    let mut global_lock = 1i32;
    let mut iface: *mut Dispatchable = std::ptr::null_mut();
    unsafe { call(factory, FACTORY_GETINTERFACE, (&raw mut iface).cast(), &mut [(&raw mut global_lock).cast()]) };
    cache.store(iface, Ordering::SeqCst);
    iface
}

/// Winamp's api_memmgr: album art buffers must come from it, since Winamp frees them.
fn memmgr() -> *mut Dispatchable {
    api(MEMMGR_GUID, &MEMMGR)
}

/// Tells Winamp the art for `file` changed, so skins ask for it again. Needed because Winamp
/// asks when a track starts, before the Spotify cover URL is known.
// ponytail: issued from a worker thread; gen_ff's art reload is thread-agnostic (threadpool +
// APC to the main thread). Marshal to the UI thread if another consumer turns out not to be.
pub fn art_updated(file: &str) {
    let syscb = api(SYSCB_GUID, &SYSCB);
    if syscb.is_null() {
        return;
    }
    let wide = to_wide(file);
    let (mut event, mut msg) = (SYSCALLBACK_META, METADATA_ART_UPDATED);
    let (mut param1, mut param2) = (wide.as_ptr() as isize, 0isize);
    let mut ret = 0i32;
    let params: &mut [*mut c_void] =
        &mut [(&raw mut event).cast(), (&raw mut msg).cast(), (&raw mut param1).cast(), (&raw mut param2).cast()];
    unsafe { call(syscb, SYSCB_ISSUECALLBACK, (&raw mut ret).cast(), params) };
}

/// Copies `data` into a Winamp-owned buffer.
fn winamp_alloc(data: &[u8]) -> Option<*mut c_void> {
    let mm = memmgr();
    if mm.is_null() {
        return None;
    }
    let mut size = data.len();
    let mut p: *mut c_void = std::ptr::null_mut();
    unsafe { call(mm, MEMMGR_SYSMALLOC, (&raw mut p).cast(), &mut [(&raw mut size).cast()]) };
    if p.is_null() {
        return None;
    }
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), p.cast(), data.len()) };
    Some(p)
}

unsafe extern "thiscall" fn factory_dispatch(
    _this: *mut Dispatchable, msg: i32, retval: *mut c_void, _params: *mut *mut c_void, _nparam: i32,
) -> i32 {
    if retval.is_null() {
        return 0;
    }
    unsafe {
        match msg {
            FACTORY_GETSERVICETYPE => *retval.cast::<u32>() = SERVICE_TYPE,
            FACTORY_GETSERVICENAME => *retval.cast::<*const u8>() = c"SpotiTube Album Art".as_ptr().cast(),
            FACTORY_GETGUID => *retval.cast::<Guid>() = PROVIDER_GUID,
            FACTORY_GETINTERFACE => *retval.cast::<*mut Dispatchable>() = ptr(&PROVIDER),
            FACTORY_SUPPORTNONLOCKING | FACTORY_RELEASEINTERFACE | FACTORY_SERVICENOTIFY => *retval.cast::<i32>() = 1,
            FACTORY_GETTESTSTRING => *retval.cast::<*const u16>() = std::ptr::null(),
            _ => return 0,
        }
    }
    1
}

unsafe extern "thiscall" fn provider_dispatch(
    _this: *mut Dispatchable, msg: i32, retval: *mut c_void, params: *mut *mut c_void, nparam: i32,
) -> i32 {
    if retval.is_null() {
        return 0;
    }
    // SAFETY: Winamp passes `nparam` pointers to the arguments, per svc_albumArtProvider.h.
    let arg = |i: usize| unsafe { *params.add(i) };
    unsafe {
        match msg {
            PROVIDER_PROVIDERTYPE => *retval.cast::<i32>() = ALBUMARTPROVIDER_TYPE_EMBEDDED,
            PROVIDER_ISMINE if nparam >= 1 => {
                let file = wide_to_string(*arg(0).cast::<*const u16>());
                *retval.cast::<bool>() = crate::is_supported(&file);
            }
            PROVIDER_GETALBUMARTDATA if nparam >= 5 => {
                let file = wide_to_string(*arg(0).cast::<*const u16>());
                let kind = wide_to_string(*arg(1).cast::<*const u16>());
                let out = (*arg(2).cast::<*mut *mut c_void>(), *arg(3).cast::<*mut usize>(), *arg(4).cast::<*mut *mut u16>());
                *retval.cast::<i32>() = get_album_art(&file, &kind, out);
            }
            PROVIDER_SETALBUMARTDATA | PROVIDER_DELETEALBUMART => *retval.cast::<i32>() = ALBUMARTPROVIDER_READONLY,
            _ => return 0,
        }
    }
    1
}

unsafe fn get_album_art(file: &str, kind: &str, (bits, len, mime): (*mut *mut c_void, *mut usize, *mut *mut u16)) -> i32 {
    if !(kind.is_empty() || kind.eq_ignore_ascii_case("cover")) || bits.is_null() || len.is_null() {
        return ALBUMARTPROVIDER_FAILURE;
    }
    let Some(url) = cover_url(file) else { return ALBUMARTPROVIDER_FAILURE };
    let Some(image) = image(&url) else { return ALBUMARTPROVIDER_FAILURE };
    let Some(data) = winamp_alloc(&image) else { return ALBUMARTPROVIDER_FAILURE };
    unsafe {
        *bits = data;
        *len = image.len();
        if !mime.is_null() {
            let wide = to_wide(MIME_TYPE);
            let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
            *mime = winamp_alloc(bytes).map_or(std::ptr::null_mut(), |p| p.cast());
        }
    }
    ALBUMARTPROVIDER_SUCCESS
}

fn image(url: &str) -> Option<Vec<u8>> {
    if let Some((u, bytes)) = LAST_IMAGE.lock().unwrap().as_ref()
        && u == url
    {
        return Some(bytes.clone());
    }
    let bytes = download(url, MAX_IMAGE_BYTES)?;
    *LAST_IMAGE.lock().unwrap() = Some((url.to_owned(), bytes.clone()));
    Some(bytes)
}

#[link(name = "wininet")]
unsafe extern "system" {
    fn InternetOpenW(agent: *const u16, access: u32, proxy: *const u16, bypass: *const u16, flags: u32) -> *mut c_void;
    fn InternetOpenUrlW(h: *mut c_void, url: *const u16, headers: *const u16, len: u32, flags: u32, ctx: usize) -> *mut c_void;
    fn InternetReadFile(h: *mut c_void, buf: *mut c_void, n: u32, read: *mut u32) -> i32;
    fn InternetSetOptionW(h: *mut c_void, option: u32, buf: *const c_void, len: u32) -> i32;
    fn InternetCloseHandle(h: *mut c_void) -> i32;
}

const INTERNET_OPEN_TYPE_PRECONFIG: u32 = 0;
const INTERNET_FLAG_NO_UI: u32 = 0x0000_0200;
const INTERNET_OPTION_CONNECT_TIMEOUT: u32 = 2;
const INTERNET_OPTION_RECEIVE_TIMEOUT: u32 = 6;
/// Winamp may ask on the UI thread; don't hang it on a slow network.
const DOWNLOAD_TIMEOUT_MS: u32 = 3000;
const READ_CHUNK: usize = 16 * 1024;

/// Synchronous HTTP GET through WinINet (built into Windows, honors the system proxy).
/// Fails if the body is larger than `max_bytes`.
pub fn download(url: &str, max_bytes: usize) -> Option<Vec<u8>> {
    let agent = to_wide("in_spotitube");
    let url = to_wide(url);
    unsafe {
        let net = InternetOpenW(agent.as_ptr(), INTERNET_OPEN_TYPE_PRECONFIG, std::ptr::null(), std::ptr::null(), 0);
        if net.is_null() {
            return None;
        }
        let timeout = DOWNLOAD_TIMEOUT_MS;
        for option in [INTERNET_OPTION_CONNECT_TIMEOUT, INTERNET_OPTION_RECEIVE_TIMEOUT] {
            InternetSetOptionW(net, option, (&raw const timeout).cast(), size_of::<u32>() as u32);
        }
        let req = InternetOpenUrlW(net, url.as_ptr(), std::ptr::null(), 0, INTERNET_FLAG_NO_UI, 0);
        let mut out = Vec::new();
        let mut ok = !req.is_null();
        let mut chunk = vec![0u8; READ_CHUNK];
        while ok {
            let mut n = 0u32;
            ok = InternetReadFile(req, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut n) != 0;
            if !ok || n == 0 || out.len() + n as usize > max_bytes {
                ok = ok && n == 0;
                break;
            }
            out.extend_from_slice(&chunk[..n as usize]);
        }
        if !req.is_null() {
            InternetCloseHandle(req);
        }
        InternetCloseHandle(net);
        (ok && !out.is_empty()).then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn downloads_cover_live() {
        let bytes = download("https://i.ytimg.com/vi/jNQXAC9IVRw/hqdefault.jpg", MAX_IMAGE_BYTES).unwrap();
        assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF], "JPEG magic");
    }
}
