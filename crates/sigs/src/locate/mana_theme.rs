//! Desktop `ManaTheme` global state and `SemanticColors::get_mut` locators.
//!
//! `App::global::<ManaTheme>` is the unique exact loader of the panic type name
//! `"mana_gpui::theme::ManaTheme"`. Its opening `TypeId` load identifies every
//! `ManaTheme` global accessor, and `App::set_global::<ManaTheme>` is the one
//! that allocates `size_of::<ManaTheme>()` bytes and stores the `dyn Any`
//! vtable `(drop_in_place, size, align)`.
//!
//! `SemanticColors::get` and `SemanticColors::get_mut` are the two functions
//! that load the exact token literals `"background-base-low"`,
//! `"background-base-lower"`, and `"background-base-lowest"`; `get` loads a
//! 4-byte `u32` color in its epilogue while `get_mut` returns `&mut u32`
//! directly.

use crate::{
   Found,
   index::{
      Func,
      Index,
      Op,
      Value,
   },
   one,
};

/// Byte offsets inside `ManaTheme` and its `Arc` payloads, decoded from
/// `drop_in_place::<ManaTheme>` and `PartialEq::eq::<ManaTheme>` rather than
/// hardcoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManaThemeLayout {
   /// `ThemeRef` enum (`tag` at +0, `payload` at +8).
   pub theme_ref:        usize,
   /// Active `SemanticColorsRef` enum (`tag` at +0, `payload` at +8).
   pub colors_ref:       usize,
   /// `Option<SemanticColorsRef>` pre-filter colors (`u64::MAX` = `None`).
   pub pre_filter:       usize,
   /// Byte offset of `SemanticColors` inside `ArcInner<Theme>` (tag 1).
   pub arc_theme_colors: usize,
   /// Byte offset of `SemanticColors` inside `ArcInner<SemanticColors>` (tag 2).
   pub arc_colors_data:  usize,
   /// `base_theme: u8` (`0` = Dark, `1` = Light, `2` = Darker, `3` = Midnight).
   pub base_theme:       usize,
   /// `Option<GradientTheme>` discriminant byte offset inside `ManaTheme`.
   pub gradient_tag:     usize,
   /// `Option<GradientTheme>::None` discriminant value.
   pub gradient_none:    u8,
}

/// Everything the desktop theming engine needs to hook `ManaTheme` and mutate
/// its `SemanticColors` table without hardcoding struct sizes or field offsets.
#[derive(Clone, Copy, Debug)]
pub struct ManaTheme {
   /// `gpui::App::set_global::<ManaTheme>(&mut self, global: ManaTheme)`.
   pub set_global:    usize,
   /// Per-window `ManaTheme` pool sync helper (`fn(*const c_void, *mut c_void) -> bool`)
   /// that copies `App::global::<ManaTheme>` into every window's cached theme slot.
   pub sync_pool:     usize,
   /// Post-`set_global` sync helper (`fn(&mut gpui::App)`) that rebuilds
   /// Discord's GPUI component `Theme` global from the installed `ManaTheme`.
   pub sync_theme:    usize,
   /// `core::ptr::drop_in_place::<ManaTheme>` from the `dyn Any` vtable.
   pub drop_in_place: usize,
   /// `size_of::<ManaTheme>()` read from the `dyn Any` vtable.
   pub size:          usize,
   /// `SemanticColors::get_mut(&mut self, name: &str) -> Option<&mut u32>`.
   pub get_mut:       usize,
   /// `size_of::<SemanticColors>()` decoded from `get_mut`'s max token offset.
   pub colors_size:   usize,
   /// Field and `Arc` payload offsets decoded from `drop_in_place` and `eq`.
   pub layout:        ManaThemeLayout,
}

/// Locates `ManaTheme` global hooks, `SemanticColors::get_mut`, and the
/// `ManaTheme` / `SemanticColors` layout.
pub fn mana_theme(ix: &Index<'_>) -> Found<ManaTheme> {
   let global = locate_global(ix)?;
   let type_ids = candidate_type_ids(ix, global);
   if type_ids.is_empty() {
      return Err(format!("ManaTheme TypeId not found in {global:?}"));
   }
   let (set_global, drop_in_place, size) = locate_set_global(ix, &type_ids)?;
   let sync_theme = locate_sync_theme(ix, set_global)?;
   let sync_pool = locate_sync_pool(ix, set_global, sync_theme, global.start)?;
   let get_mut = locate_get_mut(ix)?;
   let colors_size = decode_colors_size(ix, get_mut)?;
   let (theme_ref, colors_ref, pre_filter) = decode_drop_layout(ix, drop_in_place, size)?;
   let (base_theme, arc_theme_colors, arc_colors_data, gradient_tag, gradient_none) =
      decode_eq_layout(ix, global, size)?;
   Ok(ManaTheme {
      set_global,
      sync_pool,
      sync_theme,
      drop_in_place,
      size,
      get_mut,
      colors_size,
      layout: ManaThemeLayout {
         theme_ref,
         colors_ref,
         pre_filter,
         arc_theme_colors,
         arc_colors_data,
         base_theme,
         gradient_tag,
         gradient_none,
      },
   })
}

/// The unique function that loads the exact `"mana_gpui::theme::ManaTheme"`
/// string address (used in `App::global`'s missing-global panic message).
fn locate_global<'ix>(ix: &'ix Index<'ix>) -> Found<Func<'ix>> {
   const TYPE_NAME: &[u8] = b"mana_gpui::theme::ManaTheme";

   let mut candidates = Vec::new();
   for hit in ix.text(TYPE_NAME) {
      candidates.extend(ix.loaders(hit..hit + 1));
   }
   candidates.sort_unstable_by_key(|func| func.start);
   candidates.dedup_by_key(|func| func.start);
   one("App::global::<ManaTheme>", &candidates)
}

/// Collects all distinct non-code data addresses loaded near the start of
/// `App::global::<ManaTheme>` as candidate `TypeId::of::<ManaTheme>()` slots.
fn candidate_type_ids(ix: &Index<'_>, global: Func<'_>) -> Vec<usize> {
   const HEAD: usize = 32;

   let steps = global.steps();
   let head = &steps[..steps.len().min(HEAD)];
   let mut ids = Vec::new();
   let mut pending_addr: Option<(u8, usize)> = None;
   for step in head {
      match step.op {
         Op::Global {
            target,
            store: false,
         } if !ix.image.is_code(target) => {
            ids.push(target);
         },
         Op::Addr { reg, target } if !ix.image.is_code(target) => {
            if let Some((prev_reg, prev_target)) = pending_addr {
               if prev_reg == reg {
                  // Resolved `adrp + add` pair on arm64: prefer the combined address.
                  ids.push(target);
                  pending_addr = None;
                  continue;
               }
               ids.push(prev_target);
            }
            pending_addr = Some((reg, target));
         },
         Op::Load { .. } | Op::CmpMem { .. } | Op::Branch(_) | Op::Call(_) => {
            if let Some((_, target)) = pending_addr.take() {
               ids.push(target);
            }
         },
         _ => {},
      }
   }
   if let Some((_, target)) = pending_addr {
      ids.push(target);
   }
   ids.sort_unstable();
   ids.dedup();
   ids
}

/// Checks whether `vtable` is a valid `dyn Any` vtable for `ManaTheme`:
/// `[drop_in_place (code), size (0x80..=0x200, 8-aligned), align (8)]`.
fn read_mana_vtable(ix: &Index<'_>, vtable: usize) -> Option<(usize, usize)> {
   if ix.image.is_code(vtable) {
      return None;
   }
   let width = ix.image.pointer_width;
   let drop_fn = ix.image.read_usize(vtable)?;
   let size = ix.image.read_usize(vtable + width)?;
   let align = ix.image.read_usize(vtable + 2 * width)?;
   if ix.image.is_code(drop_fn)
      && (0x80..=0x200).contains(&size)
      && size.is_multiple_of(8)
      && align == 8
   {
      Some((drop_fn, size))
   } else {
      None
   }
}

/// Verifies that `steps` captures `arg1` (the owned `ManaTheme` pointer) near
/// entry and executes an exact `__rust_alloc(size, 8)` call (`arg0 = size`,
/// `arg1 = 8`, followed by `Call`) before storing the `dyn Any` vtable.
fn allocates_and_captures_arg1(ix: &Index<'_>, steps: &[crate::index::Step], size: usize) -> bool {
   let (Some(arg0), Some(arg1)) = (ix.isa.arg(0), ix.isa.arg(1)) else {
      return false;
   };
   let head = &steps[..steps.len().min(20)];
   let captures_arg1 = head.iter().any(|step| {
      matches!(
         step.op,
         Op::Move { src, .. } if src == arg1
      ) || matches!(
         step.op,
         Op::Store {
            value: Value::Reg(reg),
            size: 8,
            ..
         } if reg == arg1
      )
   });
   if !captures_arg1 {
      return false;
   }

   // Match `arg0 = size`, `arg1 = 8` feeding the same `Call` (Rust's
   // `__rust_alloc(size, 8)`), with no intervening `Call`.
   let mut arg0_is_size = false;
   let mut arg1_is_align = false;
   for step in steps {
      match step.op {
         Op::Imm { reg, value } => {
            if reg == arg0 {
               arg0_is_size = value == size as u64;
            } else if reg == arg1 {
               arg1_is_align = value == 8;
            }
         },
         Op::Call(_) => {
            if arg0_is_size && arg1_is_align {
               return true;
            }
            arg0_is_size = false;
            arg1_is_align = false;
         },
         Op::Move { dst, .. } | Op::Load { reg: dst, .. } | Op::Offset { reg: dst, .. } => {
            if dst == arg0 {
               arg0_is_size = false;
            } else if dst == arg1 {
               arg1_is_align = false;
            }
         },
         _ => {},
      }
   }
   false
}

/// Finds `App::set_global::<ManaTheme>` among the functions loading any
/// candidate `TypeId`, verifying that it allocates `size_of::<ManaTheme>()`
/// bytes (`align == 8`), copies `size` bytes from arg 1 into the allocation,
/// notifies global observers (tag 3), and stores the `dyn Any` vtable.
fn locate_set_global(ix: &Index<'_>, type_ids: &[usize]) -> Found<(usize, usize, usize)> {
   let mut found = Vec::new();
   for &type_id in type_ids {
      for func in ix.loaders(type_id..type_id + 1) {
         let steps = func.steps();
         let notifies = steps.iter().any(|step| {
            matches!(
               step.op,
               Op::Imm { value: 3, .. }
                  | Op::Store {
                     value: Value::Imm(3),
                     size: 4,
                     ..
                  }
            )
         });
         if !notifies {
            continue;
         }
         let vtable = steps.iter().find_map(|step| {
            let Op::Addr { target, .. } = step.op else {
               return None;
            };
            let (drop_fn, size) = read_mana_vtable(ix, target)?;
            allocates_and_captures_arg1(ix, &steps, size).then_some((drop_fn, size))
         });
         if let Some((drop_fn, size)) = vtable {
            found.push((func.start, drop_fn, size));
         }
      }
   }
   found.sort_unstable();
   found.dedup();
   one("App::set_global::<ManaTheme>", &found)
}

/// Locates `SemanticColors::get_mut(&mut self, name: &str) -> Option<&mut u32>`.
///
/// Intersecting exact-boundary loaders of three non-prefix canonical Mana
/// tokens yields `[SemanticColors::get, SemanticColors::get_mut]`. `get` loads
/// a 4-byte `u32` color from `self` on its return path, whereas `get_mut`
/// computes `&mut u32` via pointer arithmetic without a 4-byte load.
fn locate_get_mut(ix: &Index<'_>) -> Found<usize> {
   // Three non-prefix canonical Mana tokens so substring hits cannot alias
   // another token literal in Rust's non-NUL-terminated rodata pool.
   const TOKENS: [&[u8]; 3] = [
      b"background-base-lowest",
      b"background-brand",
      b"text-default",
   ];

   let exact_loaders = |needle: &[u8]| -> Vec<usize> {
      let mut starts = Vec::new();
      for hit in ix.text(needle) {
         starts.extend(ix.loaders(hit..hit + 1).into_iter().map(|func| func.start));
      }
      starts.sort_unstable();
      starts.dedup();
      starts
   };

   let mut common = exact_loaders(TOKENS[0]);
   for token in &TOKENS[1..] {
      let next = exact_loaders(token);
      common.retain(|start| next.binary_search(start).is_ok());
   }

   // Count 4-byte `u32` loads across the full function (excluding string
   // comparison / length checks in the prefix is unnecessary because `get` and
   // `get_mut` share the exact same match tree, so `get` has strictly more
   // 4-byte color loads than `get_mut`, or `get_mut` has zero).
   let mut scored: Vec<(usize, usize)> = common
      .into_iter()
      .filter_map(|start| {
         let func = ix.func(start).ok()?;
         let u32_loads = func
            .steps()
            .iter()
            .filter(|step| matches!(step.op, Op::Load { size: 4, .. }))
            .count();
         Some((u32_loads, start))
      })
      .collect();
   scored.sort_unstable();

   match scored.as_slice() {
      [(0, start)] => Ok(*start),
      [(lo, start), (hi, _)] if lo < hi => Ok(*start),
      _ => {
         let addrs: Vec<usize> = scored.into_iter().map(|(_, s)| s).collect();
         Err(format!(
            "SemanticColors::get_mut could not be distinguished among {addrs:#x?}"
         ))
      },
   }
}

/// Decodes `size_of::<SemanticColors>()` from the largest 4-byte-aligned field
/// offset added to `self` inside `SemanticColors::get_mut`.
fn decode_colors_size(ix: &Index<'_>, get_mut: usize) -> Found<usize> {
   let arg0 = ix
      .isa
      .arg(0)
      .ok_or_else(|| "ISA has no arg0 register".to_owned())?;
   let func = ix.func(get_mut)?;
   let steps = func.steps();

   let mut self_regs = vec![arg0];
   for step in steps.iter().take(20) {
      if let Op::Move { dst, src } = step.op
         && src == arg0
         && !self_regs.contains(&dst)
      {
         self_regs.push(dst);
      }
   }

   let max_off = steps
      .iter()
      .filter_map(|step| {
         let Op::Offset { base, offset, .. } = step.op else {
            return None;
         };
         let off = usize::try_from(offset).ok()?;
         (self_regs.contains(&base) && (4..0x10_000).contains(&off) && off.is_multiple_of(4))
            .then_some(off)
      })
      .max()
      .filter(|&off| off >= 0x400)
      .ok_or_else(|| format!("SemanticColors max token offset not found in {get_mut:#x}"))?;

   Ok((max_off + 4).next_multiple_of(8))
}

/// Tracks registers and stack spill slots holding `arg0` through a small
/// function.
fn collect_self_regs(
   steps: &[crate::index::Step],
   arg0: u8,
) -> (Vec<u8>, Vec<(u8, isize)>) {
   let mut regs = vec![arg0];
   let mut spills = Vec::new();
   for step in steps {
      match step.op {
         Op::Move { dst, src } if regs.contains(&src) && !regs.contains(&dst) => {
            regs.push(dst);
         },
         Op::Store {
            value: Value::Reg(src),
            base,
            offset,
            size: 8,
         } if regs.contains(&src) => {
            let slot = (base, offset);
            if !spills.contains(&slot) {
               spills.push(slot);
            }
         },
         Op::Load {
            reg,
            base,
            offset,
            size: 8,
         } if spills.contains(&(base, offset)) && !regs.contains(&reg) => {
            regs.push(reg);
         },
         _ => {},
      }
   }
   (regs, spills)
}

/// Decodes `(theme_ref, colors_ref, pre_filter)` offsets from
/// `drop_in_place::<ManaTheme>` and its tail-called inner drop helper.
fn decode_drop_layout(
   ix: &Index<'_>,
   drop_in_place: usize,
   size: usize,
) -> Found<(usize, usize, usize)> {
   let arg0 = ix
      .isa
      .arg(0)
      .ok_or_else(|| "ISA has no arg0 register".to_owned())?;
   let func = ix.func(drop_in_place)?;
   let steps = func.steps();
   let (self_regs, _) = collect_self_regs(&steps, arg0);

   // `theme_ref`: first 8-byte tag check on `self` before the Arc atomic dec.
   let theme_ref = steps
      .iter()
      .take_while(|step| !matches!(step.op, Op::Atomic { .. }))
      .find_map(|step| {
         let (Op::Load {
            base,
            offset,
            size: 8,
            ..
         }
         | Op::CmpMem {
            base,
            offset,
            size: 8,
            ..
         }) = step.op
         else {
            return None;
         };
         let off = usize::try_from(offset).ok()?;
         (self_regs.contains(&base) && off < size && off.is_multiple_of(8)).then_some(off)
      })
      .ok_or_else(|| format!("ManaTheme::theme_ref offset not found in {drop_in_place:#x}"))?;

   // `colors_ref` + `inner_drop`: offset added to `self` before the tail jump.
   let mut last_off: Option<usize> = None;
   let mut colors_and_inner: Option<(usize, usize)> = None;
   for step in &steps {
      match step.op {
         Op::Offset { base, offset, .. } if self_regs.contains(&base) => {
            if let Ok(off) = usize::try_from(offset)
               && off > theme_ref
               && off < size
               && off.is_multiple_of(8)
            {
               last_off = Some(off);
            }
         },
         Op::Jump(target) if !(func.start..func.end).contains(&target) => {
            if let Some(off) = last_off {
               colors_and_inner = Some((off, target));
               break;
            }
         },
         _ => {},
      }
   }
   let (colors_ref, inner_addr) = colors_and_inner.ok_or_else(|| {
      format!("ManaTheme::colors_ref offset and inner drop not found in {drop_in_place:#x}")
   })?;

   // `pre_filter`: first positive 8-byte load from `inner_self` in `inner_drop`.
   let inner = ix.func(inner_addr)?;
   let inner_steps = inner.steps();
   let (inner_regs, _) = collect_self_regs(&inner_steps, arg0);
   let pre_filter_rel = inner_steps
      .iter()
      .find_map(|step| {
         let Op::Load {
            base,
            offset,
            size: 8,
            ..
         } = step.op
         else {
            return None;
         };
         let off = usize::try_from(offset).ok()?;
         (inner_regs.contains(&base)
            && off > 0
            && colors_ref + off < size
            && off.is_multiple_of(8))
         .then_some(off)
      })
      .ok_or_else(|| format!("ManaTheme::pre_filter offset not found in {inner_addr:#x}"))?;

   Ok((theme_ref, colors_ref, colors_ref + pre_filter_rel))
}

/// Locates the `fn(&mut gpui::App)` sync helper that every caller of
/// `App::set_global::<ManaTheme>` invokes immediately afterwards to rebuild
/// Discord's GPUI component `Theme` global (colors, fonts, shadows) from the
/// newly installed `ManaTheme`.
fn locate_sync_theme(ix: &Index<'_>, set_global: usize) -> Found<usize> {
   let func = ix.func(set_global)?;
   let shadow_loaders: Vec<usize> = ix
      .literal(b"shadow-low")
      .iter()
      .map(|f| f.start)
      .collect();
   let mut next_callees = Vec::new();
   for caller in func.callers() {
      let calls = caller.calls();
      for pair in calls.windows(2) {
         if pair[0] == set_global && shadow_loaders.binary_search(&pair[1]).is_ok() {
            next_callees.push(pair[1]);
         }
      }
   }
   next_callees.sort_unstable();
   next_callees.dedup();
   one("ManaTheme sync_theme", &next_callees)
}

/// Locates the `fn(*const c_void, *mut c_void) -> bool` pool sync helper
/// invoked right after `sync_theme` in `set_global`'s callers, which reads
/// `App::global::<ManaTheme>` and updates every window's cached `ManaTheme`.
fn locate_sync_pool(
   ix: &Index<'_>,
   set_global: usize,
   sync_theme: usize,
   global: usize,
) -> Found<usize> {
   let func = ix.func(set_global)?;
   let mut candidates = Vec::new();
   for caller in func.callers() {
      let calls = caller.calls();
      for (idx, pair) in calls.windows(2).enumerate() {
         if pair[0] == set_global && pair[1] == sync_theme {
            for &callee in calls[idx + 2..].iter().take(3) {
               if let Ok(cf) = ix.func(callee)
                  && cf.calls().contains(&global)
               {
                  candidates.push(callee);
               }
            }
         }
      }
   }
   candidates.sort_unstable();
   candidates.dedup();
   one("ManaTheme sync_pool", &candidates)
}

/// Decodes `(arc_colors_data, arc_theme_colors)` from the tag-2 / tag-1 branch
/// inside `PartialEq::eq::<ManaTheme>`.
fn find_arc_offsets(steps: &[crate::index::Step]) -> Option<(usize, usize)> {
   steps.windows(2).enumerate().find_map(|(idx, pair)| {
      let (Op::Cmp { value: 2, .. }, Op::Branch(target)) = (pair[0].op, pair[1].op) else {
         return None;
      };
      let after_branch = &steps[idx + 2..];
      let tag2_off = after_branch
         .iter()
         .take_while(|step| step.at < target)
         .find_map(|step| {
            let Op::Offset { offset, .. } = step.op else {
               return None;
            };
            let off = usize::try_from(offset).ok()?;
            (off > 0 && off < 0x200 && off.is_multiple_of(8)).then_some(off)
         })?;
      let tag1_off = after_branch
         .iter()
         .skip_while(|step| step.at < target)
         .take(4)
         .find_map(|step| {
            let Op::Offset { offset, .. } = step.op else {
               return None;
            };
            let off = usize::try_from(offset).ok()?;
            (off > 0 && off < 0x200 && off.is_multiple_of(8)).then_some(off)
         })?;
      (tag1_off != tag2_off).then_some((tag2_off, tag1_off))
   })
}

/// Decodes `(gradient_tag, gradient_none)` from the `Option<GradientTheme>`
/// discriminant comparison against `None` inside `PartialEq::eq::<ManaTheme>`.
fn find_gradient_tag(
   steps: &[crate::index::Step],
   arg0: u8,
   arg1: u8,
   size: usize,
) -> Option<(usize, u8)> {
   steps.iter().enumerate().find_map(|(idx, step)| {
      match step.op {
         Op::CmpMem {
            base,
            offset,
            size: 1,
            value,
         } if base == arg0 => {
            let off = usize::try_from(offset).ok()?;
            let none = u8::try_from(value).ok().filter(|&v| v > 0 && v < 16)?;
            if off >= size {
               return None;
            }
            let pair_matches = steps[idx + 1..].iter().take(4).any(|next| {
               matches!(
                  next.op,
                  Op::CmpMem {
                     base: b2,
                     offset: o2,
                     size: 1,
                     value: v2,
                  } if b2 == arg1 && o2 == offset && v2 == value
               )
            });
            pair_matches.then_some((off, none))
         },
         Op::Load {
            reg,
            base,
            offset,
            size: 1,
         } if base == arg0 => {
            let off = usize::try_from(offset).ok()?;
            if off >= size {
               return None;
            }
            let Op::Cmp { reg: cmp_reg, value } = steps.get(idx + 1)?.op else {
               return None;
            };
            let none = u8::try_from(value).ok().filter(|&v| v > 0 && v < 16)?;
            if cmp_reg != reg {
               return None;
            }
            let pair_matches = steps[idx + 2..].windows(2).take(4).any(|pair| {
               matches!(
                  (pair[0].op, pair[1].op),
                  (
                     Op::Load {
                        reg: r2,
                        base: b2,
                        offset: o2,
                        size: 1,
                     },
                     Op::Cmp {
                        reg: c2,
                        value: v2,
                     },
                  ) if b2 == arg1 && o2 == offset && c2 == r2 && v2 == value
               )
            });
            pair_matches.then_some((off, none))
         },
         _ => None,
      }
   })
}

/// Decodes `(base_theme, arc_theme_colors, arc_colors_data, gradient_tag, gradient_none)`
/// from `PartialEq::eq::<ManaTheme>`, called by GPUI's `ManaTheme` global observers.
fn decode_eq_layout(
   ix: &Index<'_>,
   global: Func<'_>,
   size: usize,
) -> Found<(usize, usize, usize, usize, u8)> {
   let arg0 = ix
      .isa
      .arg(0)
      .ok_or_else(|| "ISA has no arg0 register".to_owned())?;
   let arg1 = ix
      .isa
      .arg(1)
      .ok_or_else(|| "ISA has no arg1 register".to_owned())?;

   let mut callees = Vec::new();
   for caller in global.callers() {
      if caller.size() > 1024 {
         continue;
      }
      callees.extend(caller.calls());
   }
   callees.sort_unstable();
   callees.dedup();

   let mut decoded = Vec::new();
   for callee in callees {
      let Ok(func) = ix.func(callee) else {
         continue;
      };
      if func.size() < 64 || func.size() > 1024 {
         continue;
      }
      let steps = func.steps();
      if steps.iter().any(|step| matches!(step.op, Op::Call(_))) {
         continue;
      }
      let Some(base_theme) = steps.iter().take(4).find_map(|step| {
         let Op::Load {
            base,
            offset,
            size: 1,
            ..
         } = step.op
         else {
            return None;
         };
         let off = usize::try_from(offset).ok()?;
         (base == arg0 && off < size).then_some(off)
      }) else {
         continue;
      };
      let Some((arc_colors_data, arc_theme_colors)) = find_arc_offsets(&steps) else {
         continue;
      };
      let Some((gradient_tag, gradient_none)) = find_gradient_tag(&steps, arg0, arg1, size) else {
         continue;
      };
      decoded.push((
         base_theme,
         arc_theme_colors,
         arc_colors_data,
         gradient_tag,
         gradient_none,
      ));
   }

   decoded.sort_unstable();
   decoded.dedup();
   one("ManaTheme::eq layout", &decoded)
}
