//! The composer's Nitro gift button, which `phoenix.ui.hide("composer.gift")`
//! leaves out, and on macOS `phoenix.ui.addComposerButton`'s buttons, drawn
//! ahead of the chat bar's own through a detour on the bar's render.

use std::sync::{
   Mutex,
   OnceLock,
   PoisonError,
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

use crate::accessories;

/// `jne rel32`, and `nop; jmp rel32` to the same place.
const SHOWN: [u8; 2] = [0x0F, 0x85];
const HIDDEN: [u8; 2] = [0x90, 0xE9];

static GIFT: OnceLock<usize> = OnceLock::new();
/// Plugins holding the gift button hidden.
static HIDERS: Mutex<usize> = Mutex::new(0);

/// Finds the gift button's branch from [`crate::prepare`], while the locator
/// indexes are built.
pub fn install(image: &Image<'static>) {
   match desktop::composer_gift(image) {
      Ok(site) if image.read(site, 2) == Some(&SHOWN[..]) => {
         let _ = GIFT.set(site);
      },
      Ok(site) => log::warn!("composer gift: {site:#x} is no jne"),
      Err(err) => log::warn!("composer gift: {err}, the gift button stays"),
   }
}

/// Hides the gift button until the matching `hide_gift(false)`. False when
/// this build's button was not found.
pub fn hide_gift(hide: bool) -> bool {
   #[cfg(target_os = "macos")]
   if !crate::X86_CODE {
      return hooks::hide_gift(hide);
   }
   let (Some(&site), Some(state)) = (GIFT.get(), phoenix_runtime::state()) else {
      return false;
   };
   let mut hiders = HIDERS.lock().unwrap_or_else(PoisonError::into_inner);
   let next = if hide {
      *hiders + 1
   } else {
      hiders.saturating_sub(1)
   };
   if (*hiders > 0) != (next > 0) {
      let bytes = if hide { HIDDEN } else { SHOWN };
      // SAFETY: both bytes lie in one aligned word, which the locator checked,
      // so a thread at the branch reads the old or the new opcode whole.
      if let Err(err) = unsafe { state.image.write_code(site, &bytes) } {
         log::error!("composer gift: {err}");
         return false;
      }
   }
   *hiders = next;
   true
}

/// A plugin button as the render draws it. `on` draws it as the primary
/// button, which is how a tint shows.
#[cfg_attr(
   not(target_os = "macos"),
   allow(dead_code, reason = "only the macOS chat bar draws buttons")
)]
pub struct Button {
   pub id:    u64,
   pub label: String,
   pub on:    bool,
}

static BUTTONS: Mutex<Vec<Button>> = Mutex::new(Vec::new());

/// Adds or replaces the button with `button.id` and redraws.
pub fn set(button: Button) {
   let mut buttons = BUTTONS.lock().unwrap_or_else(PoisonError::into_inner);
   match buttons.iter_mut().find(|known| known.id == button.id) {
      Some(known) => *known = button,
      None => buttons.push(button),
   }
   drop(buttons);
   accessories::invalidate_discord_windows();
}

pub fn remove(ids: &[u64]) {
   let mut buttons = BUTTONS.lock().unwrap_or_else(PoisonError::into_inner);
   let before = buttons.len();
   buttons.retain(|button| !ids.contains(&button.id));
   if buttons.len() != before {
      drop(buttons);
      accessories::invalidate_discord_windows();
   }
}

#[cfg(target_os = "macos")]
pub mod hooks {
   use std::{
      ffi::c_void,
      panic::{
         self,
         AssertUnwindSafe,
      },
      ptr,
      sync::{
         OnceLock,
         PoisonError,
         atomic::{
            AtomicBool,
            AtomicUsize,
            Ordering,
         },
      },
   };

   use phoenix_hook::image::{
      Image,
      sret_callee,
      sret_caller,
   };

   use crate::{
      accessories,
      composer::{
         BUTTONS,
         HIDERS,
      },
      gpui::{
         BtnSize,
         BtnVariant,
         RawAnyElement,
         RcFnVtbl,
         W_FULL,
         build_raw_button,
         gpui_px,
         h_row,
         push_any,
         push_div,
         push_raw_button,
         raw_div,
         style_discord_div,
         style_div,
         write_any,
      },
      js,
   };

   type RenderFn = unsafe extern "C" fn(usize, usize, usize, usize, usize) -> RawAnyElement;

   static ORIG: OnceLock<RenderFn> = OnceLock::new();

   pub fn install(image: &Image<'static>) {
      let func = match phoenix_sigs::macos::composer_buttons(image) {
         Ok(func) => func,
         Err(err) => {
            log::warn!("desktop composer: {err}, skipping");
            return;
         },
      };
      // SAFETY: runs from `prepare` before Discord's threads, and `detour` has
      // the render's signature.
      let Some(orig) = (unsafe {
         crate::hook::<RenderFn>(
            image,
            |pro| pro.frame,
            func,
            detour as *const () as usize,
            "desktop composer: hook failed",
         )
      }) else {
         return;
      };
      let _ = ORIG.set(orig);
      log::info!("desktop composer: hooked the button row @ {func:#x}");
      install_hide(image, func);
   }

   /// Read on every `Div::id`, where [`HIDERS`]'s lock would be too slow.
   static GIFT_HIDDEN: AtomicBool = AtomicBool::new(false);

   /// [`super::hide_gift`] through the `Div::id` detour. False when it is not
   /// hooked.
   pub fn hide_gift(hide: bool) -> bool {
      if ORIG_ID.get().is_none() {
         return false;
      }
      let mut hiders = HIDERS.lock().unwrap_or_else(PoisonError::into_inner);
      *hiders = if hide {
         *hiders + 1
      } else {
         hiders.saturating_sub(1)
      };
      GIFT_HIDDEN.store(*hiders > 0, Ordering::Relaxed);
      drop(hiders);
      accessories::invalidate_discord_windows();
      true
   }

   static GIFT_WRAP_ID: AtomicUsize = AtomicUsize::new(0);
   /// The interned key of the wrapper's `ElementId`, its second word, once
   /// Discord interned it.
   static GIFT_KEY: AtomicUsize = AtomicUsize::new(0);

   /// Both hooked functions return through `x8`, taken here the x86 way, the
   /// slot first.
   type InternFn = unsafe extern "C" fn(*mut [usize; 4], *const u8, usize) -> *mut [usize; 4];
   type IdSetterFn = unsafe extern "C" fn(*mut u8, *mut u8, *const [usize; 4]) -> *mut u8;

   static ORIG_INTERN: OnceLock<InternFn> = OnceLock::new();
   static ORIG_ID: OnceLock<IdSetterFn> = OnceLock::new();

   /// Hooks `target` with `detour` through the sret thunks, returning the
   /// original the same way.
   fn hook_sret<F: Copy>(image: &Image<'static>, target: usize, detour: usize) -> Option<F> {
      const { assert!(size_of::<F>() == size_of::<usize>()) };
      let detour = sret_callee(detour)?;
      let what = format!("desktop composer: hooking {target:#x} failed");
      // SAFETY: runs from `prepare` before Discord's threads, and the callee
      // thunk hands the detour the call the x86 way.
      let trampoline = unsafe { crate::hook(image, |pro| pro.frame, target, detour, &what) }?;
      // SAFETY: the caller thunk calls the trampoline the arm64 way.
      sret_caller(trampoline).map(|caller| unsafe { std::mem::transmute_copy::<usize, F>(&caller) })
   }

   fn install_hide(image: &Image<'static>, composer: usize) {
      let gift = match phoenix_sigs::macos::gift_button(image, composer) {
         Ok(gift) => gift,
         Err(err) => {
            log::warn!("desktop composer: {err}, hiding stays off");
            return;
         },
      };
      GIFT_WRAP_ID.store(gift.wrap_id, Ordering::Relaxed);
      let Some(intern) = hook_sret(image, gift.intern, intern_detour as *const () as usize) else {
         return;
      };
      let _ = ORIG_INTERN.set(intern);
      let Some(setter) = hook_sret(image, gift.id_setter, id_detour as *const () as usize) else {
         return;
      };
      let _ = ORIG_ID.set(setter);
      log::info!("desktop composer: hiding through {gift:x?}");
   }

   unsafe extern "C" fn intern_detour(
      out: *mut [usize; 4],
      text: *const u8,
      len: usize,
   ) -> *mut [usize; 4] {
      let orig = *ORIG_INTERN
         .get()
         .expect("the detour only runs once ORIG_INTERN is set");
      // SAFETY: forwards the call unchanged.
      let out = unsafe { orig(out, text, len) };
      if text as usize == GIFT_WRAP_ID.load(Ordering::Relaxed) {
         // SAFETY: `out` is the `ElementId` just built.
         GIFT_KEY.store(unsafe { (*out)[1] }, Ordering::Relaxed);
      }
      out
   }

   unsafe extern "C" fn id_detour(out: *mut u8, div: *mut u8, id: *const [usize; 4]) -> *mut u8 {
      let orig = *ORIG_ID
         .get()
         .expect("the detour only runs once ORIG_ID is set");
      // SAFETY: forwards the call unchanged.
      let out = unsafe { orig(out, div, id) };
      let key = GIFT_KEY.load(Ordering::Relaxed);
      // SAFETY: `id` is the `ElementId` Discord passed.
      if GIFT_HIDDEN.load(Ordering::Relaxed) && key != 0 && unsafe { (*id)[1] } == key {
         // SAFETY: `out` is the `Stateful<Div>` the setter just built.
         unsafe { style_discord_div(out, |style| style.set(style.at.display, DISPLAY_NONE)) };
      }
      out
   }

   /// `taffy::Display::None`, after `Block`, `Flex` and `Grid`.
   const DISPLAY_NONE: u8 = 3;

   unsafe extern "C" fn detour(
      a0: usize,
      a1: usize,
      a2: usize,
      a3: usize,
      a4: usize,
   ) -> RawAnyElement {
      let orig = *ORIG.get().expect("the detour only runs once ORIG is set");
      // SAFETY: forwards every argument to the render.
      let mut out = unsafe { orig(a0, a1, a2, a3, a4) };
      let _ = panic::catch_unwind(AssertUnwindSafe(|| decorate(&raw mut out)));
      out
   }

   /// Puts the plugin buttons in a row ahead of the element `sret` holds.
   fn decorate(sret: *mut RawAnyElement) {
      let buttons = BUTTONS.lock().unwrap_or_else(PoisonError::into_inner);
      if buttons.is_empty() {
         return;
      }
      let Some(mut row) = h_row(4.0, false) else {
         return;
      };
      for button in buttons.iter() {
         let variant = if button.on {
            BtnVariant::Primary
         } else {
            BtnVariant::Secondary
         };
         let id = format!("sunbird-composer-{}", button.id);
         if let Some(built) = build_raw_button(
            &id,
            &button.label,
            variant,
            BtnSize::Small,
            &raw const PRESS_VTBL,
            button.id,
         ) {
            row = push_raw_button(row, built);
         }
      }
      drop(buttons);
      let Some(mut bar) = raw_div() else {
         return;
      };
      // The bar fills its parent, so it shrinks to the width the buttons leave.
      style_div(&mut row, |style| style.length(style.at.size[0], W_FULL));
      style_div(&mut bar, |style| {
         style.length(style.at.size[0], W_FULL);
         style.length(style.at.min_size[0], gpui_px(0.0));
         style.float(style.at.flex_shrink, 1.0);
      });
      // SAFETY: moves the rendered element out of `sret`, which gets it back or
      // the row holding it.
      let mut orig = unsafe { ptr::read(sret) };
      if !push_any(&mut bar, &mut orig) {
         // SAFETY: puts back the element read above, which nothing took.
         unsafe { ptr::write(sret, orig) };
         return;
      }
      write_any(push_div(row, bar), sret);
   }

   static PRESS_VTBL: RcFnVtbl = RcFnVtbl {
      drop_in_place: ptr::null(),
      size:          size_of::<u64>(),
      align:         align_of::<u64>(),
      call_once:     press as *const c_void,
      call_mut:      press as *const c_void,
      call:          press as *const c_void,
      extra:         ptr::null(),
   };

   unsafe extern "C" fn press(
      id: *const u64,
      _event: *const c_void,
      _window: *mut c_void,
      _app: *mut c_void,
   ) {
      if id.is_null() {
         return;
      }
      // SAFETY: GPUI passes a pointer to the `u64` payload of the `Rc` that
      // `build_raw_button` allocated.
      let id = unsafe { *id };
      let _ = panic::catch_unwind(|| js::ui::press_composer_button(id));
   }
}
