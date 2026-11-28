//! Live `.theme.css` themes, applied to GPUI's `Scene` right before the
//! renderer draws it, so every desktop renderer shows the same colors.
//!
//! The renderer's `draw(&mut self, scene: &Scene)` is observed; on entry its
//! quads, monochrome sprites (icons) and subpixel sprites (text) are recolored
//! in place. Their element layouts are GPUI's `#[repr(C)]` structs, which the
//! shaders read as is.

use std::{
   path::{
      Path,
      PathBuf,
   },
   sync::{
      LazyLock,
      OnceLock,
      PoisonError,
      RwLock,
   },
};

#[cfg(windows)]
use phoenix_hook::image::Image;
use phoenix_sigs::desktop::SceneDraw;

use crate::theme_css::{
   Theme,
   parse_theme_css,
};

static SCENE: OnceLock<SceneDraw> = OnceLock::new();
static THEME: LazyLock<RwLock<Theme>> = LazyLock::new(RwLock::default);
static HOME: OnceLock<PathBuf> = OnceLock::new();

/// Observes the renderer's `draw`. Elsewhere themes stay off, since the
/// locator only decodes Win64 builds.
#[cfg(windows)]
pub fn install(image: &Image<'static>) {
   draw::install(image);
}

#[must_use]
pub fn is_supported() -> bool {
   crate::mana_theme::is_supported() || SCENE.get().is_some()
}

/// Applies `.theme.css` text now. Returns whether it themes anything.
pub fn set_css(css: &str) -> bool {
   if crate::mana_theme::is_supported() {
      // Keep `--radius-scale` active on Windows's Scene hook.
      let radius_scale = parse_theme_css(css).radius_scale;
      let scene_active = cfg!(windows) && radius_scale.is_some();
      *THEME.write().unwrap_or_else(PoisonError::into_inner) = Theme {
         active: scene_active,
         radius_scale,
         ..Theme::default()
      };
      return crate::mana_theme::set_css(css) || scene_active;
   }
   let theme = parse_theme_css(css);
   let active = theme.active;
   *THEME.write().unwrap_or_else(PoisonError::into_inner) = theme;
   crate::modal::force_redraw();
   active
}

/// The `--radius-scale` a theme file in `dir` sets, if it reads and parses.
pub fn file_radius(dir: &Path, file: &str) -> Option<f32> {
   phoenix_themes::read_contained_theme(dir, file)
      .ok()
      .and_then(|raw| phoenix_themes::ThemeSpec::parse_file(file, &raw).ok())
      .and_then(|spec| spec.radius_scale)
}

/// Syncs `--radius-scale` from an applied theme file onto Windows's Scene hook.
/// Returns whether `--radius-scale` is active on the Scene hook.
pub fn sync_file_radius(dir: &Path, file: &str) -> bool {
   let radius_scale = file_radius(dir, file);
   let scene_active = cfg!(windows) && radius_scale.is_some();
   *THEME.write().unwrap_or_else(PoisonError::into_inner) = Theme {
      active: scene_active,
      radius_scale,
      ..Theme::default()
   };
   if scene_active {
      crate::modal::force_redraw();
   }
   scene_active
}

/// Syncs `--radius-scale` from `<home>/themes.json` onto Windows's Scene hook
/// at startup and when restoring persisted themes after `clear()` or unload.
pub fn sync_persisted_radius(home: &Path) {
   let _ = HOME.set(home.to_owned());
   let cfg = phoenix_themes::load_config(&home.join("themes.json"));
   let radius_scale =
      phoenix_themes::load_active_spec(&home.join("themes"), &cfg).and_then(|s| s.radius_scale);
   let scene_active = cfg!(windows) && radius_scale.is_some();
   *THEME.write().unwrap_or_else(PoisonError::into_inner) = Theme {
      active: scene_active,
      radius_scale,
      ..Theme::default()
   };
   if scene_active {
      crate::modal::force_redraw();
   }
}

pub fn clear() {
   if crate::mana_theme::is_supported() {
      if let Some(home) = HOME.get() {
         sync_persisted_radius(home);
      } else {
         *THEME.write().unwrap_or_else(PoisonError::into_inner) = Theme::default();
      }
      crate::mana_theme::restore_persisted();
      return;
   }
   set_css("");
}

/// A `.theme.css` or `.theme.json` file in the themes folder.
pub struct ThemeFile {
   pub file:        String,
   pub name:        String,
   pub description: Option<String>,
   pub author:      Option<String>,
}

/// `*.theme.css` and `*.theme.json` files in `dir`, named by their metadata.
#[must_use]
pub fn list(dir: &Path) -> Vec<ThemeFile> {
   phoenix_themes::list_themes(dir)
      .into_iter()
      .map(|entry| {
         ThemeFile {
            file:        entry.file,
            name:        entry.meta.name,
            description: entry.meta.description,
            author:      entry.meta.author,
         }
      })
      .collect()
}

pub fn open_dir(dir: &Path) {
   let _ = std::fs::create_dir_all(dir);
   let opener = if cfg!(windows) {
      "explorer"
   } else if cfg!(target_os = "macos") {
      "open"
   } else {
      "xdg-open"
   };
   let _ = std::process::Command::new(opener)
      .arg(PathBuf::from(dir))
      .spawn();
}

#[cfg(windows)]
mod draw {
   use std::sync::{
      OnceLock,
      PoisonError,
   };

   use phoenix_hook::{
      image::Image,
      x86::{
         Observed,
         Regs,
         observe,
      },
   };
   use phoenix_sigs::desktop;

   use super::{
      SCENE,
      THEME,
   };
   use crate::theme_css::{
      Hsla,
      QuadRole,
      Theme,
      remap_border_color,
      remap_gradient_stops,
      remap_mono_color,
      remap_quad_color,
   };

   /// `gpui::Quad`: `order`, `border_style`, `bounds`, `content_mask`,
   /// `background`, `border_color`, `corner_radii`, `border_widths`.
   static HOOK: OnceLock<Observed> = OnceLock::new();

   mod quad {
      pub const SIZE: usize = 160;
      pub const BOUNDS: usize = 8;
      pub const CONTENT_MASK: usize = 24;
      pub const BACKGROUND_TAG: usize = 40;
      pub const SOLID: usize = 48;
      /// The two `LinearColorStop`s of a gradient, an `Hsla` and a percentage.
      pub const STOPS: [usize; 2] = [68, 88];
      pub const BORDER: usize = 112;
      pub const RADII: usize = 128;
   }

   /// `gpui::MonochromeSprite` and `gpui::SubpixelSprite` share this layout.
   mod sprite {
      pub const SIZE: usize = 112;
      pub const COLOR: usize = 72;
   }

   pub(super) fn install(image: &Image<'static>) {
      let found = match desktop::scene_draw(image) {
         Ok(found) => found,
         Err(err) => {
            log::warn!("desktop themes: {err}, themes disabled");
            return;
         },
      };
      // SAFETY: `scene_draw` found a function start, and `prepare` runs before
      // Discord's render thread exists.
      match unsafe { observe(image, found.draw, on_draw, 0) } {
         Ok(hook) => {
            let _ = SCENE.set(found);
            let _ = HOOK.set(hook);
            log::info!(
               "desktop themes: observing the renderer's draw @ {:#x}",
               found.draw
            );
         },
         Err(err) => log::warn!("desktop themes: observing draw: {err}"),
      }
   }

   /// A `Vec<T>` in the scene, as `(ptr, len)`.
   ///
   /// # Safety
   ///
   /// `scene + at` is a live `Vec`.
   const unsafe fn list_at(scene: usize, at: usize) -> (usize, usize) {
      // SAFETY: forwarded from the caller; `Vec` is `cap`, `ptr`, `len`.
      unsafe {
         let words = (scene + at) as *const usize;
         (words.add(1).read(), words.add(2).read())
      }
   }

   extern "C" fn on_draw(_: usize, regs: &Regs) {
      let theme = THEME.read().unwrap_or_else(PoisonError::into_inner);
      let Some(found) = SCENE.get().filter(|_| theme.active) else {
         return;
      };
      let Ok(scene) = usize::try_from(regs.args()[1]) else {
         return;
      };
      // A panic must not unwind into the renderer.
      let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
         // SAFETY: `draw` received a live `&Scene` that nothing else touches
         // until it returns, and `scene_draw` decoded where its lists sit.
         unsafe {
            let (ptr, len) = list_at(scene, found.quads);
            recolor_quads(
               std::slice::from_raw_parts_mut(ptr as *mut u8, len * quad::SIZE),
               &theme,
            );
            for at in [found.mono, found.subpixel] {
               let (ptr, len) = list_at(scene, at);
               let sprites = std::slice::from_raw_parts_mut(ptr as *mut u8, len * sprite::SIZE);
               for sprite in sprites.as_chunks_mut::<{ sprite::SIZE }>().0 {
                  edit_hsla(sprite, sprite::COLOR, |c| remap_mono_color(&theme, c));
               }
            }
         }
      }));
   }

   fn read4(buf: &[u8], at: usize) -> [f32; 4] {
      std::array::from_fn(|i| {
         f32::from_le_bytes(
            buf[at + 4 * i..at + 4 * i + 4]
               .try_into()
               .unwrap_or_default(),
         )
      })
   }

   fn write4(buf: &mut [u8], at: usize, v: [f32; 4]) {
      for (i, x) in v.iter().enumerate() {
         buf[at + 4 * i..at + 4 * i + 4].copy_from_slice(&x.to_le_bytes());
      }
   }

   /// Runs `f` on the color at `at` when it is visible, and writes it back.
   fn edit_hsla(buf: &mut [u8], at: usize, f: impl FnOnce(&mut Hsla)) {
      let mut c = read_hsla(buf, at);
      if c.a > 0.005 && c.a <= 1.001 {
         f(&mut c);
         write4(buf, at, [c.h, c.s, c.l, c.a]);
      }
   }

   fn read_hsla(buf: &[u8], at: usize) -> Hsla {
      let [h, s, l, a] = read4(buf, at);
      Hsla { h, s, l, a }
   }

   /// GPUI's large panels, told apart by size and place in the window.
   fn quad_role([x, y, w, h]: [f32; 4], (vw, vh): (f32, f32)) -> QuadRole {
      if w >= vw * 0.85 && h >= vh * 0.85 {
         QuadRole::FullWindowRoot
      } else if x <= 12.0 && (44.0..=96.0).contains(&w) && h >= vh * 0.5 {
         QuadRole::ServerRail
      } else if (140.0..=380.0).contains(&w) && h >= vh * 0.5 && (x <= 120.0 || x + w >= vw - 40.0)
      {
         QuadRole::Sidebar
      } else if y <= 56.0 && w >= vw * 0.45 && (24.0..=64.0).contains(&h) {
         QuadRole::HeaderBar
      } else if w >= vw * 0.45 && h >= vh * 0.45 {
         QuadRole::MainPane
      } else {
         QuadRole::Default
      }
   }

   fn recolor_quads(quads: &mut [u8], theme: &Theme) {
      // Every quad is clipped to the window, so the content masks reach its
      // edges and never past them. Bounds can: a scrolled list taller than the
      // window would make the window look taller than it is, and the backdrop
      // under Settings then no longer reads as full-window and turns opaque.
      let window =
         quads
            .as_chunks::<{ quad::SIZE }>()
            .0
            .iter()
            .fold((0.0f32, 0.0f32), |(w, h), q| {
               let [x, y, mw, mh] = read4(q, quad::CONTENT_MASK);
               (w.max(x + mw), h.max(y + mh))
            });
      let sized = window.0 >= 400.0 && window.1 >= 300.0;
      for q in quads.as_chunks_mut::<{ quad::SIZE }>().0 {
         let bounds = read4(q, quad::BOUNDS);
         let [_, _, w, h] = bounds;
         let large =
            (w >= 96.0 && h >= 48.0) || (w >= 44.0 && h >= 200.0) || (w >= 200.0 && h >= 28.0);
         let role = if sized {
            quad_role(bounds, window)
         } else {
            QuadRole::Default
         };
         let tag = u32::from_le_bytes(
            q[quad::BACKGROUND_TAG..][..4]
               .try_into()
               .unwrap_or_default(),
         );
         let is_root = role == QuadRole::FullWindowRoot;
         match tag {
            0 => {
               let [_, _, l, a] = read4(q, quad::SOLID);
               // A modal or image viewer dims the window with a near-black
               // translucent quad; recoloring it would light the window up.
               if is_root && a < 0.96 && l <= 0.15 {
                  continue;
               }
               edit_hsla(q, quad::SOLID, |c| {
                  remap_quad_color(theme, c, large, false, is_root, role);
               });
            },
            // A gradient, such as a Nitro client theme's background.
            1 => {
               let huge = w >= 200.0 && h >= 100.0;
               let mut stops = quad::STOPS.map(|at| read_hsla(q, at));
               remap_gradient_stops(theme, &mut stops, large, huge, is_root, role);
               for (at, c) in quad::STOPS.into_iter().zip(stops) {
                  write4(q, at, [c.h, c.s, c.l, c.a]);
               }
            },
            _ => {},
         }
         edit_hsla(q, quad::BORDER, |c| remap_border_color(theme, c));
         if let Some(scale) = theme.radius_scale {
            let radii =
               read4(q, quad::RADII).map(|r| if r > 0.5 && r < 24.0 { r * scale } else { r });
            write4(q, quad::RADII, radii);
         }
      }
   }
}
