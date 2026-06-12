thread 'main' (16428) panicked at crates\gpui-flowtext\src\rich_text\editor\authoritative_projection.rs:228:24:
byte offset out of bounds: the offset is 25 but the length is 1
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

thread 'main' (16428) panicked at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\core\src\panicking.rs:225:5:
panic in a function that cannot unwind
stack backtrace:
   0:     0x7ff7b01bca2e - std::backtrace_rs::backtrace::win64::trace
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\..\..\backtrace\src\backtrace\win64.rs:85
   1:     0x7ff7b01bca2e - std::backtrace_rs::backtrace::trace_unsynchronized
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\..\..\backtrace\src\backtrace\mod.rs:66
   2:     0x7ff7b01bca2e - std::sys::backtrace::_print_fmt
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\sys\backtrace.rs:74
   3:     0x7ff7b01bca2e - std::sys::backtrace::impl$0::print::impl$0::fmt
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\sys\backtrace.rs:44
   4:     0x7ff7b01cd411 - core::fmt::write
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\core\src\fmt\mod.rs:0
   5:     0x7ff7b01c33a4 - std::io::default_write_fmt
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\io\mod.rs:622
   6:     0x7ff7b01c33a4 - std::io::Write::write_fmt<std::sys::stdio::windows::Stderr>
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\io\mod.rs:1977
   7:     0x7ff7b01a24b9 - std::sys::backtrace::BacktraceLock::print
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\sys\backtrace.rs:47
   8:     0x7ff7b01a24b9 - std::panicking::default_hook::closure$0
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panicking.rs:292
   9:     0x7ff7b01b3c68 - std::panicking::default_hook
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panicking.rs:319
  10:     0x7ff7b01b3f23 - std::panicking::panic_with_hook
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panicking.rs:825
  11:     0x7ff7b01a25c4 - std::panicking::panic_handler::closure$0
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panicking.rs:691
  12:     0x7ff7b01a003f - std::sys::backtrace::__rust_end_short_backtrace<std::panicking::panic_handler::closure_env$0,never$>
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\sys\backtrace.rs:182
  13:     0x7ff7b01a2cbe - std::panicking::panic_handler
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panicking.rs:689
  14:     0x7ff7b43f7ee7 - core::panicking::panic_nounwind_fmt
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\core\src\intrinsics\mod.rs:2450
  15:     0x7ff7b43f7e61 - core::panicking::panic_nounwind
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\core\src\panicking.rs:225
  16:     0x7ff7b43f801a - core::panicking::panic_cannot_unwind
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\core\src\panicking.rs:337
  17:     0x7ff7b0ba917b - <gpui[60b3eedad06cbd3b]::platform::windows::window::WindowsWindowInner>::set_window_placement
  18:     0x7ff88d2ebb00 - _CxxFrameHandler3
  19:     0x7ff88d2d3a9d - is_exception_typeof
  20:     0x7ff88d2eae8d - _C_specific_handler
  21:     0x7ff88d2d2ce2 - is_exception_typeof
  22:     0x7ff88d2eb8f8 - _CxxFrameHandler3
  23:     0x7ff8ca80482f - _chkstk
  24:     0x7ff8ca6be433 - RtlUnwindEx
  25:     0x7ff88d2eb3d6 - _C_specific_handler
  26:     0x7ff88d2d1c20 - is_exception_typeof
  27:     0x7ff88d2d2dd8 - is_exception_typeof
  28:     0x7ff88d2eb8f8 - _CxxFrameHandler3
  29:     0x7ff8ca8047af - _chkstk
  30:     0x7ff8ca6c21c7 - RtlLocateExtendedFeature
  31:     0x7ff8ca736ea1 - RtlRaiseException
  32:     0x7ff8c7f81b6a - RaiseException
  33:     0x7ff88d2d55a9 - CxxThrowException
  34:     0x7ff7b01c357d - panic_unwind::imp::throw_exception
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\panic_unwind\src\seh.rs:368
  35:     0x7ff7b01c3455 - panic_unwind::imp::panic
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\panic_unwind\src\seh.rs:302
  36:     0x7ff7b01c3455 - panic_unwind::__rust_start_panic
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\panic_unwind\src\lib.rs:108
  37:     0x7ff7b01a2b1f - std::panicking::rust_panic
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panicking.rs:886
  38:     0x7ff7b01b3f9e - std::panicking::panic_with_hook
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panicking.rs:850
  39:     0x7ff7b01a258e - std::panicking::panic_handler::closure$0
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panicking.rs:698
  40:     0x7ff7b01a003f - std::sys::backtrace::__rust_end_short_backtrace<std::panicking::panic_handler::closure_env$0,never$>
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\sys\backtrace.rs:182
  41:     0x7ff7b01a2cbe - std::panicking::panic_handler
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panicking.rs:689
  42:     0x7ff7b43f806d - core::panicking::panic_fmt
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\core\src\panicking.rs:80
  43:     0x7ff7b4433d24 - crop[f7ea77ebc596b5ec]::rope::utils::panic_messages::byte_offset_out_of_bounds
  44:     0x7ff7b0073679 - gpui_flowtext[1119af4ad3293969]::demo::document_from_input
  45:     0x7ff7b0083dea - <gpui_flowtext[1119af4ad3293969]::rich_text::editor::RichTextEditor>::apply_authoritative_projection
  46:     0x7ff7b0084dcb - <gpui_flowtext[1119af4ad3293969]::rich_text::editor::RichTextEditor>::apply_authoritative_projection
  47:     0x7ff7b0085399 - <gpui_flowtext[1119af4ad3293969]::rich_text::editor::RichTextEditor>::apply_authoritative_source_operations
  48:     0x7ff7b00ae20e - <gpui_flowtext[1119af4ad3293969]::rich_text::editor::RichTextEditor>::insert_paragraph_break_command
  49:     0x7ff7b02b5381 - <gpui[60b3eedad06cbd3b]::app::App as gpui[60b3eedad06cbd3b]::AppContext>::update_entity::<gpui_flowtext[1119af4ad3293969]::rich_text::editor::RichTextEditor, (), <flowstate[325323f06e07739b]::workspace::workspace::Workspace>::handle_window_keybinding::{closure#2}>
  50:     0x7ff7b12d4cd9 - <flowstate[325323f06e07739b]::workspace::workspace::Workspace>::handle_window_keybinding
  51:     0x7ff7b02d0e48 - <gpui[60b3eedad06cbd3b]::app::App as gpui[60b3eedad06cbd3b]::AppContext>::update_entity::<flowstate[325323f06e07739b]::workspace::workspace::Workspace, bool, <flowstate[325323f06e07739b]::workspace::workspace::Workspace>::new::{closure#2}::{closure#0}>
  52:     0x7ff7b128ceda - <gpui[60b3eedad06cbd3b]::app::entity_map::WeakEntity<flowstate[325323f06e07739b]::workspace::workspace::Workspace>>::update::<gpui[60b3eedad06cbd3b]::app::App, bool, <flowstate[325323f06e07739b]::workspace::workspace::Workspace>::new::{closure#2}::{closure#0}>
  53:     0x7ff7b0310230 - <gpui[60b3eedad06cbd3b]::app::App>::intercept_keystrokes::<<flowstate[325323f06e07739b]::workspace::workspace::Workspace>::new::{closure#2}>::{closure#0}
  54:     0x7ff7b0c179d0 - RNvXsU_NtNtNtCsblfDRQ0yya5_5alloc11collections5btree3mapINtB5_9ExtractIfjINtNtCs8iKjKkE3RPz_4gpui12subscription10SubscriberINtNtBb_5boxed3BoxDG1_INtNtNtCsbeUSaH02EkB_4core3ops8function5FnMutTRL2_NtNtB1e_3app14KeystrokeEventQL1_NtNtB1e_6window6WindowQL0_NtB
  55:     0x7ff7b0b865a2 - RINvMs_NtCs8iKjKkE3RPz_4gpui12subscriptionINtB5_13SubscriberSetuINtNtCsblfDRQ0yya5_5alloc5boxed3BoxDG1_INtNtNtCsbeUSaH02EkB_4core3ops8function5FnMutTRL2_NtNtB7_3app14KeystrokeEventQL1_NtNtB7_6window6WindowQL0_NtB2t_3AppEEp6OutputbEL_EE6retainNCNvMsm_B2Y_B2
  56:     0x7ff7b0287152 - <gpui[60b3eedad06cbd3b]::window::Window>::dispatch_event
  57:     0x7ff7b03292e2 - <gpui[60b3eedad06cbd3b]::app::async_context::AsyncApp as gpui[60b3eedad06cbd3b]::AppContext>::update_window::<gpui[60b3eedad06cbd3b]::window::DispatchEventResult, <gpui[60b3eedad06cbd3b]::window::Window>::new::{closure#10}::{closure#0}>
  58:     0x7ff7b0271f61 - <gpui[60b3eedad06cbd3b]::elements::div::Interactivity>::paint::<<gpui[60b3eedad06cbd3b]::elements::img::Img as gpui[60b3eedad06cbd3b]::element::Element>::paint::{closure#0}>::{closure#1}::{closure#1}::{closure#0}::{closure#0}
  59:     0x7ff7b0b9240e - <gpui[60b3eedad06cbd3b]::platform::windows::vsync::VSyncProvider>::new
  60:     0x7ff7b0ba82e4 - <gpui[60b3eedad06cbd3b]::platform::windows::window::WindowsWindowInner>::set_window_placement
  61:     0x7ff8c935c396 - CallWindowProcW
  62:     0x7ff8c935a7ed - IsWindowUnicode
  63:     0x7ff7b0b29f05 - <gpui[60b3eedad06cbd3b]::platform::windows::platform::WindowsPlatform as gpui[60b3eedad06cbd3b]::platform::Platform>::run
  64:     0x7ff7b029c71b - <gpui[60b3eedad06cbd3b]::app::Application>::run::<flowstate[325323f06e07739b]::app::run_standalone::{closure#0}>
  65:     0x7ff7aff09460 - flowstate[325323f06e07739b]::app::run_standalone
  66:     0x7ff7afef69b5 - flowstate[21eb9d0653564b79]::main
  67:     0x7ff7afed4ef6 - std[bf5d8ef6a30eff61]::sys::backtrace::__rust_begin_short_backtrace::<fn(), ()>
  68:     0x7ff7afef42ac - std[bf5d8ef6a30eff61]::rt::lang_start::<()>::{closure#0}
  69:     0x7ff7b01b1c80 - std::rt::lang_start_internal::closure$0
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\rt.rs:175
  70:     0x7ff7b01b1c80 - std::panicking::catch_unwind::do_call
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panicking.rs:581
  71:     0x7ff7b01b1c80 - std::panicking::catch_unwind
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panicking.rs:544
  72:     0x7ff7b01b1c80 - std::panic::catch_unwind
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\panic.rs:359
  73:     0x7ff7b01b1c80 - std::rt::lang_start_internal
                               at /rustc/d1fc603d1788cc3c0eebdb94a45a61c4f33b1674/library\std\src\rt.rs:171
  74:     0x7ff7afeffacc - main
  75:     0x7ff7b43f145f - invoke_main
                               at D:\a\_work\1\s\src\vctools\crt\vcstartup\src\startup\exe_common.inl:78
  76:     0x7ff7b43f145f - __scrt_common_main_seh
                               at D:\a\_work\1\s\src\vctools\crt\vcstartup\src\startup\exe_common.inl:288
  77:     0x7ff8c918e957 - BaseThreadInitThunk
  78:     0x7ff8ca727c1c - RtlUserThreadStart
thread caused non-unwinding panic. aborting.
error: process didn't exit successfully: `target\release\flowstate.exe` (exit code: 0xc0000409, STATUS_STACK_BUFFER_OVERRUN)
