//! Winamp Media Library plugin: a "YouTube Search" tree item whose view searches YouTube with
//! yt-dlp (`ytsearch`) and plays or enqueues results; in_spotitube.dll then plays the links.
//! Struct layouts and message ids follow gen_ml/ml.h, gen_ml/ml_ipc_0313.h and Winamp/wa_ipc.h.

use std::ffi::c_void;
use std::io::ErrorKind;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

type Hwnd = *mut c_void;
type WndProc = unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize;

/// MLHDR_VER (5.66+): `description` is a wide string.
const MLHDR_VER: i32 = 0x17;
const WM_ML_IPC: u32 = 0x0400 + 0x1000; // WM_USER + 0x1000
const ML_MSG_TREE_ONCREATEVIEW: i32 = 0x100;
const ML_IPC_TREEITEM_ADDW: isize = 0x133;
const ML_IPC_SKINWINDOW: isize = 0x1400;
const ML_IPC_TRACKSKINNEDPOPUPEX: isize = 0x1402;
const SMS_USESKINFONT: u32 = 0x1;
const ML_IPC_IMAGELIST_ADD: isize = 0x1260 + 5;
const ML_IPC_NAVCTRL_GETIMAGELIST: isize = 0x1280 + 7;
const SRC_TYPE_HBITMAP: u32 = 0x03;
const ISF_FORCE_BPP: u32 = 0x10;
/// Tree icon: drawn white on black, MLIF_FILTER1 maps it to the skin's item colors like the
/// stock icons. The tag (anything but the reserved MLTREEIMAGE_* values 0-5) links it to the item.
const ICON_SIZE: i32 = 16;
const ICON_BPP: u16 = 24;
const ICON_TAG: isize = 0x5354_5953;
const DIB_RGB_COLORS: u32 = 0;
const SKINNEDWND_TYPE_WINDOW: u32 = 0x1;
const SKINNEDWND_TYPE_LISTVIEW: u32 = 0x5;
const SKINNEDWND_TYPE_BUTTON: u32 = 0x6;
const SKINNEDWND_TYPE_EDIT: u32 = 0x8;
const SWS_USESKINFONT: u32 = 0x1;
const SWS_USESKINCOLORS: u32 = 0x2;
const SWS_USESKINCURSORS: u32 = 0x4;
const SWLVS_FULLROWSELECT: u32 = 0x0001_0000;
const SWLVS_DOUBLEBUFFER: u32 = 0x0002_0000;
const SWLVS_ALTERNATEITEMS: u32 = 0x0004_0000;
const ML_IPC_SKIN_WADLG_GETFUNC: isize = 0x600;
/// ML_IPC_SKIN_WADLG_GETFUNC selector for `int WADlg_getColor(int idx)`.
const WADLG_FUNC_GETCOLOR: usize = 1;
/// wa_dlg.h: window background color index.
const WADLG_WNDBG: i32 = 2;
const SKIN_STYLE: u32 = SWS_USESKINFONT | SWS_USESKINCOLORS | SWS_USESKINCURSORS;

const WM_WA_IPC: u32 = 0x0400;
const IPC_PLAYFILEW: isize = 1100;
const IPC_SETPLAYLISTPOS: isize = 121;
const IPC_GETLISTLENGTH: isize = 124;
/// Main window commands for the Stop and Play buttons. IPC_STARTPLAY can't be used to start a
/// given entry: Winamp's BeginPlayback() resets the playlist position to 0 first.
const WINAMP_BUTTON_STOP: usize = 40047;
const WINAMP_BUTTON_PLAY: usize = 40045;
const WM_COMMAND: u32 = 0x0111;

const WM_CREATE: u32 = 0x0001;
const WM_SIZE: u32 = 0x0005;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_NOTIFY: u32 = 0x004E;
const WM_GETDLGCODE: u32 = 0x0087;
const WM_KEYDOWN: u32 = 0x0100;
const WM_CHAR: u32 = 0x0102;
const WM_APP: u32 = 0x8000;
const WM_SEARCH_DONE: u32 = WM_APP + 1;
const VK_RETURN: usize = 0x0D;
const DLGC_WANTALLKEYS: isize = 0x4;
const BN_CLICKED: usize = 0;
const NM_DBLCLK: i32 = -3;
const NM_RCLICK: i32 = -5;
const MF_STRING: u32 = 0x0;
const MF_GRAYED: u32 = 0x1;
const MF_SEPARATOR: u32 = 0x800;
const TPM_RIGHTBUTTON: u32 = 0x2;
const TPM_RETURNCMD: u32 = 0x100;
const SW_SHOWNORMAL: i32 = 1;
const GWLP_WNDPROC: i32 = -4;
const MB_ICONERROR: u32 = 0x10;

const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const ES_AUTOHSCROLL: u32 = 0x80;
const LVS_REPORT: u32 = 0x1;
const LVS_SHOWSELALWAYS: u32 = 0x8;
const LVM_DELETEALLITEMS: u32 = 0x1009;
const LVM_GETNEXTITEM: u32 = 0x100C;
const LVM_SETCOLUMNWIDTH: u32 = 0x101E;
const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = 0x1036;
const LVM_INSERTITEMW: u32 = 0x104D;
const LVM_INSERTCOLUMNW: u32 = 0x1061;
const LVM_SETITEMTEXTW: u32 = 0x1074;
const LVS_EX_FULLROWSELECT: isize = 0x20;
const LVNI_SELECTED: isize = 0x2;
const LVCF_WIDTH: u32 = 0x2;
const LVCF_TEXT: u32 = 0x4;
const LVIF_TEXT: u32 = 0x1;

const ID_QUERY: usize = 100;
const ID_SEARCH: usize = 101;
const ID_LIST: usize = 102;
const ID_PLAY: usize = 103;
const ID_ENQUEUE: usize = 104;
const ID_OPEN_BROWSER: usize = 105;
const ID_ENQUEUE_ALL: usize = 106;

const MARGIN: i32 = 8;
const ROW_HEIGHT: i32 = 24;
const BUTTON_WIDTH: i32 = 80;
const COLUMNS: [&str; 3] = ["Title", "Channel", "Length"];
const LENGTH_COLUMN_WIDTH: i32 = 60;
/// Room for the vertical scrollbar, so columns never force a horizontal one.
const SCROLLBAR_ALLOWANCE: i32 = 24;
/// Channel column's share of the width left after Length.
const CHANNEL_SHARE_PERCENT: i32 = 30;
const MAX_RESULTS: u32 = 30;
const SEARCH_FIELDS: usize = 4;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const QUERY_MAX: usize = 512;

#[repr(C)]
struct MlPlugin {
    version: i32,
    description: *const u16,
    init: unsafe extern "C" fn() -> i32,
    quit: unsafe extern "C" fn(),
    message_proc: unsafe extern "C" fn(i32, isize, isize, isize) -> isize,
    // Filled in by the library.
    hwnd_winamp: Hwnd,
    hwnd_library: Hwnd,
    h_dll_instance: *mut c_void,
    service: *mut c_void,
}

#[repr(C)]
struct MlTreeItemW {
    size: usize,
    id: usize,
    parent_id: usize,
    title: *mut u16,
    title_len: usize,
    has_children: i32,
    image_index: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// {8A054D1F-E38E-4cc0-A78A-F216F059F57E}
const MLIF_FILTER1_UID: Guid = Guid { data1: 0x8a05_4d1f, data2: 0xe38e, data3: 0x4cc0, data4: [0xa7, 0x8a, 0xf2, 0x16, 0xf0, 0x59, 0xf5, 0x7e] };

#[repr(C)]
struct MlImageSource {
    size: i32,
    instance: *mut c_void,
    name: *const u16,
    bpp: u32,
    x_src: i32,
    y_src: i32,
    cx_src: i32,
    cy_src: i32,
    cx_dst: i32,
    cy_dst: i32,
    source_type: u32,
    flags: u32,
}

#[repr(C)]
struct MlImageListItem {
    size: i32,
    image_list: *mut c_void,
    source: *mut MlImageSource,
    filter: Guid,
    tag: isize,
    index: i32,
}

#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_ppm: i32,
    y_ppm: i32,
    clr_used: u32,
    clr_important: u32,
}

#[repr(C)]
struct MlSkinnedPopup {
    size: i32,
    menu: *mut c_void,
    flags: u32,
    x: i32,
    y: i32,
    hwnd: Hwnd,
    tpm_params: *mut c_void,
    image_list: *mut c_void,
    width: i32,
    skin_style: u32,
    custom_proc: *mut c_void,
    custom_param: usize,
}

#[repr(C)]
#[derive(Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
struct MlSkinWindow {
    hwnd: Hwnd,
    skin_type: u32,
    style: u32,
}

#[repr(C)]
struct EnqueueFileWithMetaW {
    filename: *const u16,
    title: *const u16,
    ext: *const u16,
    length_secs: i32,
}

#[repr(C)]
struct NmHdr {
    hwnd_from: Hwnd,
    id_from: usize,
    code: i32,
}

#[repr(C)]
#[derive(Default)]
struct LvColumnW {
    mask: u32,
    fmt: i32,
    cx: i32,
    text: usize,
    text_max: i32,
    sub_item: i32,
    image: i32,
    order: i32,
    cx_min: i32,
    cx_default: i32,
    cx_ideal: i32,
}

#[repr(C)]
#[derive(Default)]
struct LvItemW {
    mask: u32,
    item: i32,
    sub_item: i32,
    state: u32,
    state_mask: u32,
    text: usize,
    text_max: i32,
    image: i32,
    lparam: isize,
    indent: i32,
    group_id: i32,
    columns: u32,
    pu_columns: usize,
    pi_col_fmt: usize,
    group: i32,
}

#[repr(C)]
struct WndClassW {
    style: u32,
    wnd_proc: WndProc,
    cls_extra: i32,
    wnd_extra: i32,
    instance: *mut c_void,
    icon: *mut c_void,
    cursor: *mut c_void,
    background: *mut c_void,
    menu_name: *const u16,
    class_name: *const u16,
}

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateSolidBrush(color: u32) -> *mut c_void;
    fn DeleteObject(object: *mut c_void) -> i32;
    fn CreateDIBSection(
        hdc: *mut c_void, info: *const BitmapInfoHeader, usage: u32, bits: *mut *mut u8, section: *mut c_void, offset: u32,
    ) -> *mut c_void;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassW(class: *const WndClassW) -> u16;
    fn UnregisterClassW(name: *const u16, instance: *mut c_void) -> i32;
    fn CreateWindowExW(
        ex_style: u32, class: *const u16, name: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32,
        parent: Hwnd, menu: usize, instance: *mut c_void, param: *mut c_void,
    ) -> Hwnd;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn CallWindowProcW(proc_: isize, hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn SetWindowLongW(hwnd: Hwnd, index: i32, value: isize) -> isize;
    fn SendMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn GetDlgItem(hwnd: Hwnd, id: i32) -> Hwnd;
    fn GetParent(hwnd: Hwnd) -> Hwnd;
    fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
    fn MoveWindow(hwnd: Hwnd, x: i32, y: i32, w: i32, h: i32, repaint: i32) -> i32;
    fn GetWindowTextW(hwnd: Hwnd, text: *mut u16, max: i32) -> i32;
    fn SetWindowTextW(hwnd: Hwnd, text: *const u16) -> i32;
    fn EnableWindow(hwnd: Hwnd, enable: i32) -> i32;
    fn FillRect(hdc: *mut c_void, rect: *const Rect, brush: *mut c_void) -> i32;
    fn MessageBoxW(hwnd: Hwnd, text: *const u16, caption: *const u16, kind: u32) -> i32;
    fn CreatePopupMenu() -> *mut c_void;
    fn AppendMenuW(menu: *mut c_void, flags: u32, id: usize, text: *const u16) -> i32;
    fn DestroyMenu(menu: *mut c_void) -> i32;
    fn GetCursorPos(point: *mut Point) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(
        hwnd: Hwnd, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32,
    ) -> *mut c_void;
}

const fn wide<const N: usize>(s: &str) -> [u16; N] {
    let bytes = s.as_bytes();
    let mut out = [0u16; N];
    let mut i = 0;
    while i < bytes.len() {
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}

static DESCRIPTION: [u16; 32] = wide("SpotiTube YouTube Search");
static CLASS_NAME: [u16; 24] = wide("SpotiTubeSearchView");
static TREE_TITLE: [u16; 16] = wide("YouTube Search");

static mut PLUGIN: MlPlugin = MlPlugin {
    version: MLHDR_VER,
    description: DESCRIPTION.as_ptr(),
    init,
    quit,
    message_proc,
    hwnd_winamp: std::ptr::null_mut(),
    hwnd_library: std::ptr::null_mut(),
    h_dll_instance: std::ptr::null_mut(),
    service: std::ptr::null_mut(),
};

/// Tree item id assigned by the library.
static TREE_ID: AtomicUsize = AtomicUsize::new(0);
/// Edit control's original window procedure (subclassed for Enter-to-search).
static EDIT_PROC: AtomicIsize = AtomicIsize::new(0);
/// Latest search; results of older searches are dropped.
static SEARCH_ID: AtomicU64 = AtomicU64::new(0);
/// Finished search (id, result) waiting for the view thread to pick it up.
type Pending = Option<(u64, Result<Vec<Video>, String>)>;
static PENDING: Mutex<Pending> = Mutex::new(None);
static RESULTS: Mutex<Vec<Video>> = Mutex::new(Vec::new());
/// Last query, restored when the view is recreated.
static QUERY: Mutex<String> = Mutex::new(String::new());
/// The tree icon bitmap; the library copies it on each (re)load, so it lives until quit.
static ICON: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

#[derive(Clone, Debug, PartialEq)]
struct Video {
    id: String,
    title: String,
    channel: String,
    /// -1 when unknown (live streams).
    length_secs: i32,
}

impl Video {
    fn url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn winampGetMediaLibraryPlugin() -> *mut c_void {
    (&raw mut PLUGIN).cast()
}

// `&PLUGIN` would be a reference to a `static mut` the library writes to; go through a raw pointer.
#[allow(clippy::deref_addrof)]
fn plugin() -> &'static MlPlugin {
    // SAFETY: the library fills the struct before calling init.
    unsafe { &*(&raw const PLUGIN) }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn show_error(parent: Hwnd, msg: &str) {
    let text = to_wide(msg);
    let caption = to_wide("YouTube Search");
    unsafe { MessageBoxW(parent, text.as_ptr(), caption.as_ptr(), MB_ICONERROR) };
}

unsafe extern "C" fn init() -> i32 {
    let p = plugin();
    let class = WndClassW {
        style: 0,
        wnd_proc: view_proc,
        cls_extra: 0,
        wnd_extra: 0,
        instance: p.h_dll_instance,
        icon: std::ptr::null_mut(),
        cursor: std::ptr::null_mut(),
        background: std::ptr::null_mut(),
        menu_name: std::ptr::null(),
        class_name: CLASS_NAME.as_ptr(),
    };
    let mut item = MlTreeItemW {
        size: size_of::<MlTreeItemW>(),
        id: 0,
        parent_id: 0,
        title: TREE_TITLE.as_ptr().cast_mut(),
        title_len: 0,
        has_children: 0,
        image_index: if add_tree_icon() { ICON_TAG as i32 } else { 0 },
    };
    unsafe {
        if RegisterClassW(&class) == 0 {
            return 1;
        }
        SendMessageW(p.hwnd_library, WM_ML_IPC, (&raw mut item) as usize, ML_IPC_TREEITEM_ADDW);
    }
    TREE_ID.store(item.id, Ordering::SeqCst);
    0
}

unsafe extern "C" fn quit() {
    unsafe {
        UnregisterClassW(CLASS_NAME.as_ptr(), plugin().h_dll_instance);
        let icon = ICON.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !icon.is_null() {
            DeleteObject(icon);
        }
    }
}

/// A play button in a rounded box, YouTube-style.
fn icon_pixel(x: i32, y: i32) -> bool {
    let in_box = (1..=14).contains(&x) && (3..=12).contains(&y);
    let corner = (x == 1 || x == 14) && (y == 3 || y == 12);
    // Right-pointing triangle, base at x=6 (rows 5-10), apex at x=10; measured in half pixels.
    let dy = (2 * y + 1 - ICON_SIZE).abs();
    let in_triangle = (6..=10).contains(&x) && dy * 5 <= (11 - x) * 6;
    in_box && !corner && !in_triangle
}

/// Adds the tree icon to the navigation image list; true on success.
fn add_tree_icon() -> bool {
    let header = BitmapInfoHeader {
        size: size_of::<BitmapInfoHeader>() as u32,
        width: ICON_SIZE,
        height: -ICON_SIZE, // top-down
        planes: 1,
        bit_count: ICON_BPP,
        compression: 0,
        size_image: 0,
        x_ppm: 0,
        y_ppm: 0,
        clr_used: 0,
        clr_important: 0,
    };
    let mut bits: *mut u8 = std::ptr::null_mut();
    unsafe {
        let bitmap = CreateDIBSection(std::ptr::null_mut(), &header, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if bitmap.is_null() || bits.is_null() {
            return false;
        }
        // 16 px * 3 bytes = 48: rows are already DWORD-aligned.
        let row_bytes = (ICON_SIZE * 3) as usize;
        let pixels = std::slice::from_raw_parts_mut(bits, row_bytes * ICON_SIZE as usize);
        for y in 0..ICON_SIZE {
            for x in 0..ICON_SIZE {
                let v = if icon_pixel(x, y) { 0xFF } else { 0x00 };
                let i = y as usize * row_bytes + x as usize * 3;
                pixels[i..i + 3].fill(v);
            }
        }
        ICON.store(bitmap, Ordering::SeqCst);

        let library = plugin().hwnd_library;
        let image_list = SendMessageW(library, WM_ML_IPC, 0, ML_IPC_NAVCTRL_GETIMAGELIST) as *mut c_void;
        if image_list.is_null() {
            return false;
        }
        let mut source = MlImageSource {
            size: size_of::<MlImageSource>() as i32,
            instance: std::ptr::null_mut(),
            name: bitmap.cast(),
            bpp: ICON_BPP as u32,
            x_src: 0,
            y_src: 0,
            cx_src: 0,
            cy_src: 0,
            cx_dst: 0,
            cy_dst: 0,
            source_type: SRC_TYPE_HBITMAP,
            flags: ISF_FORCE_BPP,
        };
        let mut item = MlImageListItem {
            size: size_of::<MlImageListItem>() as i32,
            image_list,
            source: &mut source,
            filter: MLIF_FILTER1_UID,
            tag: ICON_TAG,
            index: 0,
        };
        SendMessageW(library, WM_ML_IPC, (&raw mut item) as usize, ML_IPC_IMAGELIST_ADD) >= 0
    }
}

unsafe extern "C" fn message_proc(msg: i32, param1: isize, param2: isize, _param3: isize) -> isize {
    if msg != ML_MSG_TREE_ONCREATEVIEW || param1 as usize != TREE_ID.load(Ordering::SeqCst) {
        return 0;
    }
    let parent = param2 as Hwnd;
    let empty = to_wide("");
    unsafe {
        CreateWindowExW(
            0, CLASS_NAME.as_ptr(), empty.as_ptr(), WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN, 0, 0, 0, 0,
            parent, 0, plugin().h_dll_instance, std::ptr::null_mut(),
        ) as isize
    }
}

fn skin(hwnd: Hwnd, skin_type: u32, style: u32) {
    let mut s = MlSkinWindow { hwnd, skin_type, style };
    unsafe { SendMessageW(plugin().hwnd_library, WM_ML_IPC, (&raw mut s) as usize, ML_IPC_SKINWINDOW) };
}

unsafe fn child(parent: Hwnd, class: &str, text: &str, style: u32, id: usize) -> Hwnd {
    let class = to_wide(class);
    let text = to_wide(text);
    unsafe {
        CreateWindowExW(
            0, class.as_ptr(), text.as_ptr(), WS_CHILD | WS_VISIBLE | style, 0, 0, 0, 0, parent, id,
            plugin().h_dll_instance, std::ptr::null_mut(),
        )
    }
}

fn item(hwnd: Hwnd, id: usize) -> Hwnd {
    unsafe { GetDlgItem(hwnd, id as i32) }
}

unsafe extern "system" fn view_proc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        match msg {
            WM_CREATE => {
                create_controls(hwnd);
                0
            }
            // The library doesn't clear the area of the previous view; paint the skin's background.
            WM_ERASEBKGND => {
                let mut r = Rect::default();
                GetClientRect(hwnd, &mut r);
                let brush = CreateSolidBrush(skin_color(WADLG_WNDBG));
                FillRect(wparam as *mut c_void, &r, brush);
                DeleteObject(brush);
                1
            }
            WM_SIZE => {
                layout(hwnd);
                0
            }
            WM_COMMAND if wparam >> 16 == BN_CLICKED => {
                match wparam & 0xFFFF {
                    ID_SEARCH => start_search(hwnd),
                    ID_PLAY => add_videos(&selected(hwnd), true),
                    ID_ENQUEUE => add_videos(&selected(hwnd), false),
                    _ => {}
                }
                0
            }
            WM_NOTIFY => {
                let hdr = &*(lparam as *const NmHdr);
                if hdr.id_from == ID_LIST && hdr.code == NM_DBLCLK {
                    add_videos(&selected(hwnd), true);
                } else if hdr.id_from == ID_LIST && hdr.code == NM_RCLICK {
                    show_menu(hwnd);
                }
                0
            }
            WM_SEARCH_DONE => {
                show_results(hwnd, wparam as u64);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Edit subclass: the library's dialog navigation would otherwise swallow Enter.
unsafe extern "system" fn edit_proc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize {
    let original = EDIT_PROC.load(Ordering::SeqCst);
    unsafe {
        match msg {
            WM_GETDLGCODE => CallWindowProcW(original, hwnd, msg, wparam, lparam) | DLGC_WANTALLKEYS,
            WM_KEYDOWN if wparam == VK_RETURN => {
                start_search(GetParent(hwnd));
                0
            }
            WM_CHAR if wparam == VK_RETURN => 0, // no beep
            _ => CallWindowProcW(original, hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn create_controls(hwnd: Hwnd) {
    unsafe {
        skin(hwnd, SKINNEDWND_TYPE_WINDOW, SKIN_STYLE);
        let edit = child(hwnd, "Edit", &QUERY.lock().unwrap(), WS_TABSTOP | ES_AUTOHSCROLL, ID_QUERY);
        skin(edit, SKINNEDWND_TYPE_EDIT, SKIN_STYLE);
        EDIT_PROC.store(SetWindowLongW(edit, GWLP_WNDPROC, edit_proc as WndProc as usize as isize), Ordering::SeqCst);
        for (text, id) in [("Search", ID_SEARCH), ("Play", ID_PLAY), ("Enqueue", ID_ENQUEUE)] {
            skin(child(hwnd, "Button", text, WS_TABSTOP, id), SKINNEDWND_TYPE_BUTTON, SKIN_STYLE);
        }
        let list = child(hwnd, "SysListView32", "", WS_TABSTOP | LVS_REPORT | LVS_SHOWSELALWAYS, ID_LIST);
        SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, LVS_EX_FULLROWSELECT as usize, LVS_EX_FULLROWSELECT);
        for (i, name) in COLUMNS.iter().enumerate() {
            let text = to_wide(name);
            let col = LvColumnW { mask: LVCF_TEXT | LVCF_WIDTH, cx: LENGTH_COLUMN_WIDTH, text: text.as_ptr() as usize, ..Default::default() };
            SendMessageW(list, LVM_INSERTCOLUMNW, i, (&raw const col) as isize);
        }
        skin(list, SKINNEDWND_TYPE_LISTVIEW, SKIN_STYLE | SWLVS_FULLROWSELECT | SWLVS_DOUBLEBUFFER | SWLVS_ALTERNATEITEMS);
        fill_list(hwnd, &RESULTS.lock().unwrap());
    }
}

/// Query row on top, list in the middle, Play/Enqueue at the bottom.
unsafe fn layout(hwnd: Hwnd) {
    let mut r = Rect::default();
    unsafe {
        GetClientRect(hwnd, &mut r);
        let (w, h) = (r.right, r.bottom);
        let search_x = w - MARGIN - BUTTON_WIDTH;
        MoveWindow(item(hwnd, ID_QUERY), MARGIN, MARGIN, (search_x - 2 * MARGIN).max(0), ROW_HEIGHT, 1);
        MoveWindow(item(hwnd, ID_SEARCH), search_x, MARGIN, BUTTON_WIDTH, ROW_HEIGHT, 1);
        let list_y = 2 * MARGIN + ROW_HEIGHT;
        let bottom_y = h - MARGIN - ROW_HEIGHT;
        let list = item(hwnd, ID_LIST);
        let list_w = (w - 2 * MARGIN).max(0);
        MoveWindow(list, MARGIN, list_y, list_w, (bottom_y - MARGIN - list_y).max(0), 1);
        let [title_w, channel_w, length_w] = column_widths(list_w);
        for (i, width) in [title_w, channel_w, length_w].into_iter().enumerate() {
            SendMessageW(list, LVM_SETCOLUMNWIDTH, i, width as isize);
        }
        MoveWindow(item(hwnd, ID_PLAY), MARGIN, bottom_y, BUTTON_WIDTH, ROW_HEIGHT, 1);
        MoveWindow(item(hwnd, ID_ENQUEUE), 2 * MARGIN + BUTTON_WIDTH, bottom_y, BUTTON_WIDTH, ROW_HEIGHT, 1);
    }
}

/// Title takes what Channel and Length leave.
fn column_widths(list_w: i32) -> [i32; 3] {
    let rest = (list_w - SCROLLBAR_ALLOWANCE - LENGTH_COLUMN_WIDTH).max(0);
    let channel = rest * CHANNEL_SHARE_PERCENT / 100;
    [rest - channel, channel, LENGTH_COLUMN_WIDTH]
}

/// A color of the current Winamp skin (wa_dlg.h `WADLG_*` index) as a COLORREF.
fn skin_color(index: i32) -> u32 {
    let f = unsafe { SendMessageW(plugin().hwnd_library, WM_ML_IPC, WADLG_FUNC_GETCOLOR, ML_IPC_SKIN_WADLG_GETFUNC) };
    if f == 0 {
        return 0; // black, like the default skins
    }
    // SAFETY: gen_ml returns `int (*)(int)` for this selector.
    let get_color: unsafe extern "C" fn(i32) -> i32 = unsafe { std::mem::transmute(f) };
    unsafe { get_color(index) as u32 }
}

unsafe fn start_search(hwnd: Hwnd) {
    let mut buf = [0u16; QUERY_MAX];
    let len = unsafe { GetWindowTextW(item(hwnd, ID_QUERY), buf.as_mut_ptr(), buf.len() as i32) };
    let query = String::from_utf16_lossy(&buf[..len.max(0) as usize]).trim().to_owned();
    if query.is_empty() {
        return;
    }
    QUERY.lock().unwrap().clone_from(&query);
    let id = SEARCH_ID.fetch_add(1, Ordering::SeqCst) + 1;
    set_searching(hwnd, true);
    let target = hwnd as usize;
    std::thread::spawn(move || {
        let result = search(&query);
        *PENDING.lock().unwrap() = Some((id, result));
        // Fails harmlessly if the view was closed meanwhile.
        unsafe { PostMessageW(target as Hwnd, WM_SEARCH_DONE, id as usize, 0) };
    });
}

fn set_searching(hwnd: Hwnd, searching: bool) {
    let button = item(hwnd, ID_SEARCH);
    let text = to_wide(if searching { "Searching..." } else { "Search" });
    unsafe {
        SetWindowTextW(button, text.as_ptr());
        EnableWindow(button, (!searching) as i32);
    }
}

unsafe fn show_results(hwnd: Hwnd, id: u64) {
    let pending = PENDING.lock().unwrap().take_if(|(p, _)| *p == id);
    let Some((_, result)) = pending else { return };
    set_searching(hwnd, false);
    match result {
        Ok(videos) => {
            unsafe { fill_list(hwnd, &videos) };
            *RESULTS.lock().unwrap() = videos;
        }
        Err(msg) => show_error(hwnd, &msg),
    }
}

unsafe fn fill_list(hwnd: Hwnd, videos: &[Video]) {
    let list = item(hwnd, ID_LIST);
    unsafe {
        SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
        for (row, v) in videos.iter().enumerate() {
            let cells = [v.title.clone(), v.channel.clone(), format_length(v.length_secs)];
            for (col, text) in cells.iter().enumerate() {
                let text = to_wide(text);
                let it = LvItemW { mask: LVIF_TEXT, item: row as i32, sub_item: col as i32, text: text.as_ptr() as usize, ..Default::default() };
                let msg = if col == 0 { LVM_INSERTITEMW } else { LVM_SETITEMTEXTW };
                SendMessageW(list, msg, row, (&raw const it) as isize);
            }
        }
    }
}

/// The results selected in the list, in list order.
fn selected(hwnd: Hwnd) -> Vec<Video> {
    let list = item(hwnd, ID_LIST);
    let results = RESULTS.lock().unwrap();
    let mut videos = Vec::new();
    let mut row = -1isize;
    loop {
        row = unsafe { SendMessageW(list, LVM_GETNEXTITEM, row as usize, LVNI_SELECTED) };
        match usize::try_from(row).ok().and_then(|r| results.get(r)) {
            Some(v) => videos.push(v.clone()),
            None => break,
        }
    }
    videos
}

/// Right-click menu on the results, drawn by the library in the skin's style.
unsafe fn show_menu(hwnd: Hwnd) {
    let videos = selected(hwnd);
    let any = if videos.is_empty() { MF_GRAYED } else { MF_STRING };
    let all = if RESULTS.lock().unwrap().is_empty() { MF_GRAYED } else { MF_STRING };
    let mut cursor = Point::default();
    let command = unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        for (flags, id, text) in [
            (any, ID_PLAY, "Play"),
            (any, ID_ENQUEUE, "Enqueue"),
            (any, ID_OPEN_BROWSER, "Open in browser"),
            (MF_SEPARATOR, 0, ""),
            (all, ID_ENQUEUE_ALL, "Enqueue all results"),
        ] {
            let text = to_wide(text);
            AppendMenuW(menu, flags, id, text.as_ptr());
        }
        GetCursorPos(&mut cursor);
        let mut popup = MlSkinnedPopup {
            size: size_of::<MlSkinnedPopup>() as i32,
            menu,
            flags: TPM_RETURNCMD | TPM_RIGHTBUTTON,
            x: cursor.x,
            y: cursor.y,
            hwnd,
            tpm_params: std::ptr::null_mut(),
            image_list: std::ptr::null_mut(),
            width: 0,
            skin_style: SMS_USESKINFONT,
            custom_proc: std::ptr::null_mut(),
            custom_param: 0,
        };
        let command = SendMessageW(plugin().hwnd_library, WM_ML_IPC, (&raw mut popup) as usize, ML_IPC_TRACKSKINNEDPOPUPEX);
        DestroyMenu(menu);
        command as usize
    };
    match command {
        ID_PLAY => add_videos(&videos, true),
        ID_ENQUEUE => add_videos(&videos, false),
        ID_OPEN_BROWSER => {
            let (verb, empty) = (to_wide("open"), std::ptr::null());
            for v in &videos {
                let url = to_wide(&v.url());
                unsafe { ShellExecuteW(hwnd, verb.as_ptr(), url.as_ptr(), empty, empty, SW_SHOWNORMAL) };
            }
        }
        ID_ENQUEUE_ALL => add_videos(&RESULTS.lock().unwrap().clone(), false),
        _ => {} // cancelled
    }
}

/// Adds `videos` to Winamp's playlist; with `play`, starts the first of them.
fn add_videos(videos: &[Video], play: bool) {
    if videos.is_empty() {
        return;
    }
    let winamp = plugin().hwnd_winamp;
    unsafe {
        let first = SendMessageW(winamp, WM_WA_IPC, 0, IPC_GETLISTLENGTH);
        for v in videos {
            let (url, title) = (to_wide(&v.url()), to_wide(&v.title));
            let entry = EnqueueFileWithMetaW { filename: url.as_ptr(), title: title.as_ptr(), ext: std::ptr::null(), length_secs: v.length_secs };
            SendMessageW(winamp, WM_WA_IPC, (&raw const entry) as usize, IPC_PLAYFILEW);
        }
        if play {
            SendMessageW(winamp, WM_WA_IPC, first as usize, IPC_SETPLAYLISTPOS);
            SendMessageW(winamp, WM_COMMAND, WINAMP_BUTTON_STOP, 0);
            SendMessageW(winamp, WM_COMMAND, WINAMP_BUTTON_PLAY, 0);
        }
    }
}

/// Runs `yt-dlp ytsearchN:<query>`; one field per line since titles may contain any separator.
fn search(query: &str) -> Result<Vec<Video>, String> {
    let output = Command::new("yt-dlp")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .args(["--encoding", "utf-8", "--no-warnings", "--flat-playlist"])
        .args(["--print", "id", "--print", "duration", "--print", "channel", "--print", "title"])
        .arg(format!("ytsearch{MAX_RESULTS}:{query}"))
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err("yt-dlp not found on PATH.\nInstall: winget install yt-dlp.yt-dlp\nThen restart Winamp.".into());
        }
        Err(e) => return Err(format!("yt-dlp failed to start: {e}")),
    };
    if !output.status.success() {
        return Err(format!("yt-dlp: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(parse_results(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_results(stdout: &str) -> Vec<Video> {
    let lines: Vec<&str> = stdout.lines().collect();
    lines
        .as_chunks::<SEARCH_FIELDS>()
        .0
        .iter()
        .filter(|[id, ..]| id.len() == 11)
        .map(|[id, duration, channel, title]| Video {
            id: (*id).to_owned(),
            title: (*title).to_owned(),
            channel: if *channel == "NA" { String::new() } else { (*channel).to_owned() },
            length_secs: duration.parse::<f64>().map_or(-1, |s| s.round() as i32),
        })
        .collect()
}

fn format_length(secs: i32) -> String {
    if secs < 0 { String::new() } else { format!("{}:{:02}", secs / 60, secs % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_search_output() {
        let out = "jNQXAC9IVRw\n19.0\njawed\nMe at the zoo\nbadid\nNA\nNA\nskipped\nS020dzrrYW0\nNA\nNA\nLive | now\n";
        let videos = parse_results(out);
        assert_eq!(videos.len(), 2);
        assert_eq!(videos[0], Video { id: "jNQXAC9IVRw".into(), title: "Me at the zoo".into(), channel: "jawed".into(), length_secs: 19 });
        assert_eq!((videos[1].channel.as_str(), videos[1].length_secs, videos[1].title.as_str()), ("", -1, "Live | now"));
        assert_eq!(videos[0].url(), "https://www.youtube.com/watch?v=jNQXAC9IVRw");
        assert_eq!(format_length(61), "1:01");
        assert_eq!(format_length(-1), "");
        assert_eq!(column_widths(684), [420, 180, 60]);
        assert_eq!(column_widths(0), [0, 0, 60]);
        let art: Vec<String> = (0..ICON_SIZE)
            .map(|y| (0..ICON_SIZE).map(|x| if icon_pixel(x, y) { '#' } else { '.' }).collect())
            .collect();
        println!("{}", art.join("
"));
        assert!(icon_pixel(3, 7) && !icon_pixel(7, 7) && !icon_pixel(1, 3) && !icon_pixel(0, 0));
    }

    #[test]
    #[ignore]
    fn searches_live() {
        let videos = search("me at the zoo").unwrap();
        println!("{videos:?}");
        assert!(!videos.is_empty());
    }
}
