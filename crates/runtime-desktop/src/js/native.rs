//! `phoenix.native`, for reversing Discord from the debug console: pattern
//! scans, string and code cross references, disassembly, fault-free memory
//! reads, and live hooks that log each call's arguments to `/events`.
//!
//! Only the console's `phoenix` gets it, never a plugin's. Addresses inside
//! Discord.exe print as RVAs like `+0x6fc5220`, the same numbers `sigscan`
//! prints at the file's base, and either form is accepted back.

use phoenix_hook::{
   image::Image,
   pattern::Pattern,
   x86,
};
use rquickjs::{
   Ctx,
   Exception,
   Function,
   Object,
   Value,
   function::Opt,
};
use serde::Serialize;

use crate::{
   X86_CODE,
   os::{
      DesktopOs,
      Os,
   },
};

const MAX_LIST: usize = 64;

fn image() -> Option<&'static Image<'static>> {
   phoenix_runtime::state().map(|state| &state.image)
}

fn base() -> usize {
   image().map_or(0, Os::image_base)
}

/// `+rva` inside Discord.exe, else the absolute address.
fn place(addr: usize) -> String {
   let inside =
      image().is_some_and(|image| image.segments.iter().any(|segment| segment.contains(addr)));
   if inside {
      format!("+{:#x}", addr - base())
   } else {
      format!("{addr:#x}")
   }
}

/// Reads `+rva`, an absolute hex address, or a number. Hex below the image
/// base is an RVA too.
fn parse(ctx: &Ctx<'_>, value: &Value<'_>) -> rquickjs::Result<usize> {
   let bad = || Exception::throw_type(ctx, "an address is a number, \"0x…\" or \"+0x…\" (an RVA)");
   if let Some(number) = value.as_number() {
      #[expect(
         clippy::cast_possible_truncation,
         clippy::cast_sign_loss,
         reason = "user-space addresses fit in 47 bits, which f64 holds exactly"
      )]
      return if number >= 0.0 {
         Ok(number as usize)
      } else {
         Err(bad())
      };
   }
   let text = value.as_string().ok_or_else(bad)?.to_string()?;
   let (rva, digits) = text
      .strip_prefix('+')
      .map_or((false, text.as_str()), |rest| (true, rest));
   let digits = digits.trim_start_matches("0x");
   let value = usize::from_str_radix(digits, 16).map_err(|_| bad())?;
   let base = base();
   Ok(if rva || value < base {
      base + value
   } else {
      value
   })
}

fn json<'js>(ctx: &Ctx<'js>, value: &impl Serialize) -> rquickjs::Result<Value<'js>> {
   let text = serde_json::to_string(value)
      .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?;
   ctx.json_parse(text)
}

fn hex(bytes: &[u8]) -> String {
   bytes
      .iter()
      .map(|byte| format!("{byte:02x}"))
      .collect::<Vec<_>>()
      .join(" ")
}

#[derive(Serialize)]
struct Site {
   at:   String,
   #[serde(rename = "fn")]
   func: String,
   text: String,
}

impl From<&x86::CodeRef> for Site {
   fn from(site: &x86::CodeRef) -> Self {
      Self {
         at:   place(site.line.addr),
         func: place(site.function),
         text: site.line.text.clone(),
      }
   }
}

/// Code references from x86-64 decoding, none when Discord's code is arm64,
/// where decoding it as x86 would only list noise.
fn x86_refs(refs: impl FnOnce() -> Vec<x86::CodeRef>) -> Vec<Site> {
   if !X86_CODE {
      return Vec::new();
   }
   refs().iter().take(MAX_LIST).map(Site::from).collect()
}

/// The console's `phoenix.native`.
#[expect(clippy::too_many_lines, reason = "one setter per method, each short")]
pub fn object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
   let native = Object::new(ctx.clone())?;
   native.set("base", format!("{:#x}", base()))?;
   native.set(
      "scan",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, pattern: String| {
         let image = image().ok_or_else(|| Exception::throw_internal(&ctx, "not started"))?;
         let pattern = Pattern::leak(pattern).ok_or_else(|| {
            Exception::throw_type(&ctx, "pattern tokens are two hex digits or ??")
         })?;
         Ok::<_, rquickjs::Error>(
            image
               .scan(pattern)
               .into_iter()
               .take(MAX_LIST)
               .map(place)
               .collect::<Vec<_>>(),
         )
      })?,
   )?;
   native.set(
      "strings",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, text: String| {
         #[derive(Serialize)]
         struct Found {
            at:    String,
            loads: Vec<Site>,
         }
         let image = image().ok_or_else(|| Exception::throw_internal(&ctx, "not started"))?;
         let found = image
            .find_bytes(text.as_bytes())
            .into_iter()
            .take(16)
            .map(|at| {
               Found {
                  at:    place(at),
                  loads: x86_refs(|| x86::literal_loads(image, at)),
               }
            })
            .collect::<Vec<_>>();
         json(&ctx, &found)
      })?,
   )?;
   native.set(
      "refs",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, addr: Value<'js>| {
         #[derive(Serialize)]
         struct Refs {
            code:     Vec<Site>,
            pointers: Vec<String>,
         }
         let image = image().ok_or_else(|| Exception::throw_internal(&ctx, "not started"))?;
         let addr = parse(&ctx, &addr)?;
         let refs = Refs {
            code:     x86_refs(|| x86::code_refs(image, addr..addr + 1)),
            pointers: image
               .pointers_to(addr)
               .into_iter()
               .take(MAX_LIST)
               .map(place)
               .collect(),
         };
         json(&ctx, &refs)
      })?,
   )?;
   native.set(
      "fn",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, addr: Value<'js>| {
         let image = image().ok_or_else(|| Exception::throw_internal(&ctx, "not started"))?;
         let addr = parse(&ctx, &addr)?;
         Ok::<_, rquickjs::Error>(
            image
               .function_bounds(addr)
               .map(|(start, end)| vec![place(start), place(end)]),
         )
      })?,
   )?;
   native.set(
      "dis",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, addr: Value<'js>, count: Opt<usize>| {
            if !X86_CODE {
               return Err(Exception::throw_internal(
                  &ctx,
                  "phoenix.native.dis decodes x86-64, and this Discord is arm64",
               ));
            }
            let addr = parse(&ctx, &addr)?;
            let count = count.0.unwrap_or(20).min(500);
            let code = Os::read_memory(addr, count * 16).unwrap_or_default();
            Ok::<_, rquickjs::Error>(
               x86::disasm(&code, addr, count)
                  .into_iter()
                  .map(|line| format!("{}  {}", place(line.addr), line.text))
                  .collect::<Vec<_>>(),
            )
         },
      )?,
   )?;
   native.set(
      "read",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, addr: Value<'js>, len: Opt<usize>| {
            let addr = parse(&ctx, &addr)?;
            Ok::<_, rquickjs::Error>(
               Os::read_memory(addr, len.0.unwrap_or(64).min(4096)).map(|bytes| hex(&bytes)),
            )
         },
      )?,
   )?;
   native.set(
      "readU64",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, addr: Value<'js>| {
         let addr = parse(&ctx, &addr)?;
         Ok::<_, rquickjs::Error>(
            Os::read_memory(addr, 8)
               .and_then(|bytes| Some(u64::from_le_bytes(bytes.try_into().ok()?)))
               .map(|value| format!("{value:#x}")),
         )
      })?,
   )?;
   native.set(
      "readStr",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, addr: Value<'js>, len: usize| {
            let addr = parse(&ctx, &addr)?;
            Ok::<_, rquickjs::Error>(
               Os::read_memory(addr, len.min(1 << 16))
                  .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
            )
         },
      )?,
   )?;
   native.set(
      "hook",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, addr: Value<'js>, limit: Opt<u64>| {
            let image = image().ok_or_else(|| Exception::throw_internal(&ctx, "not started"))?;
            let addr = parse(&ctx, &addr)?;
            observe::hook(image, addr, limit.0.unwrap_or(50))
               .map_err(|err| Exception::throw_message(&ctx, &err))
         },
      )?,
   )?;
   native.set(
      "unhook",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, id: usize| {
         let image = image().ok_or_else(|| Exception::throw_internal(&ctx, "not started"))?;
         observe::unhook(image, id).map_err(|err| Exception::throw_message(&ctx, &err))
      })?,
   )?;
   native.set(
      "hooks",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| json(&ctx, &observe::list()))?,
   )?;
   Ok(native)
}

/// `phoenix.native.hook`, on `phoenix_hook::x86::observe` on Windows and
/// Linux, which saves the argument registers, and on
/// [`phoenix_hook::arm64::observe_stub`] on macOS.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
mod observe {
   use std::{
      cell::Cell,
      panic::{
         self,
         AssertUnwindSafe,
      },
      sync::{
         Mutex,
         PoisonError,
         atomic::{
            AtomicU64,
            AtomicUsize,
            Ordering,
         },
      },
   };

   use phoenix_hook::image::Image;
   #[cfg(any(windows, target_os = "linux"))]
   use phoenix_hook::x86::{
      Observed,
      Regs,
   };
   #[cfg(target_os = "macos")]
   use phoenix_hook::{
      arm64::{
         JUMP_LEN,
         OBSERVED_WORDS,
         observe_stub,
      },
      image::jit_copy,
      pattern::Pattern,
   };
   use serde::Serialize;

   use super::{
      hex,
      place,
   };
   use crate::os::{
      DesktopOs,
      Os,
   };

   const MAX_HOOKS: usize = 32;
   const PEEK: usize = 16;

   /// A native event for the debug event log.
   #[derive(Serialize)]
   #[serde(tag = "source", rename_all = "camelCase")]
   enum Trace {
      /// A call into a function `phoenix.native.hook` observes.
      Native(Hit),
   }

   /// One call into a hooked function, as `/events` shows it.
   #[derive(Serialize)]
   pub struct Hit {
      hook:   usize,
      at:     String,
      /// The first four integer argument registers.
      args:   [String; 4],
      /// The first bytes behind each argument that points at readable memory.
      peek:   [Option<String>; 4],
      caller: String,
      #[serde(flatten)]
      origin: Origin,
   }

   #[derive(Serialize)]
   #[serde(rename_all = "lowercase")]
   enum Origin {
      #[cfg(any(windows, target_os = "linux"))]
      Thread(u32),
      /// Return addresses inside Discord up the frame-pointer chain.
      #[cfg(target_os = "macos")]
      Stack(Vec<String>),
   }

   #[derive(Serialize)]
   pub struct Listed {
      id:   usize,
      at:   String,
      hits: u64,
   }

   struct Slot {
      addr:  AtomicUsize,
      hits:  AtomicU64,
      limit: AtomicU64,
   }

   static SLOTS: [Slot; MAX_HOOKS] = [const {
      Slot {
         addr:  AtomicUsize::new(0),
         hits:  AtomicU64::new(0),
         limit: AtomicU64::new(0),
      }
   }; MAX_HOOKS];
   /// Where each observed call continues, set once the jump is written.
   #[cfg(target_os = "macos")]
   static TRAMPOLINES: [AtomicUsize; MAX_HOOKS] = [const { AtomicUsize::new(0) }; MAX_HOOKS];

   /// What undoes each hook: the stub on x86, the bytes it overwrote on
   /// macOS.
   #[cfg(any(windows, target_os = "linux"))]
   type Undo = Observed;
   #[cfg(target_os = "macos")]
   type Undo = [u8; JUMP_LEN];

   static HOOKS: Mutex<[Option<Undo>; MAX_HOOKS]> = Mutex::new([const { None }; MAX_HOOKS]);

   /// Observes the function at `addr`, logging its first `limit` calls.
   /// Returns the hook's id.
   pub fn hook(image: &Image<'static>, addr: usize, limit: u64) -> Result<usize, String> {
      let mut hooks = HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
      if let Some(id) = (0..MAX_HOOKS)
         .find(|&id| hooks[id].is_some() && SLOTS[id].addr.load(Ordering::Relaxed) == addr)
      {
         return Err(format!("{} is hooked already as {id}", place(addr)));
      }
      let id = hooks
         .iter()
         .position(Option::is_none)
         .ok_or_else(|| format!("{MAX_HOOKS} hooks at most, unhook one first"))?;
      let slot = &SLOTS[id];
      slot.addr.store(addr, Ordering::Relaxed);
      slot.hits.store(0, Ordering::Relaxed);
      slot.limit.store(limit, Ordering::Relaxed);
      hooks[id] = Some(attach(image, addr, id)?);
      drop(hooks);
      log::info!("native: hooked {} as {id}", place(addr));
      Ok(id)
   }

   #[cfg(any(windows, target_os = "linux"))]
   fn attach(image: &Image<'static>, addr: usize, id: usize) -> Result<Undo, String> {
      // SAFETY: the debug console's user picks a function start. A wrong pick
      // or a thread paused in its first 14 bytes can crash Discord, which a
      // reversing tool accepts.
      unsafe { phoenix_hook::x86::observe(image, addr, on_hit, id) }.map_err(|err| err.to_string())
   }

   #[cfg(target_os = "macos")]
   fn attach(image: &Image<'static>, addr: usize, id: usize) -> Result<Undo, String> {
      let original: [u8; JUMP_LEN] = image
         .read(addr, JUMP_LEN)
         .and_then(|bytes| bytes.try_into().ok())
         .ok_or("the function is unmapped")?;
      TRAMPOLINES[id].store(0, Ordering::Release);
      let stub = u16::try_from(id)
         .ok()
         .and_then(|id| jit_copy(&observe_stub(id, on_hit as *const () as usize)))
         .ok_or("no memory for the stub")?;
      // SAFETY: the debug console's user picks a function start. A thread
      // inside its first 16 bytes can crash Discord, which a reversing tool
      // accepts.
      let trampoline = unsafe { image.hook_entry(addr, Pattern::new("00"), 0, stub) }
         .map_err(|err| err.to_string())?;
      TRAMPOLINES[id].store(trampoline, Ordering::Release);
      Ok(original)
   }

   pub fn unhook(image: &Image<'static>, id: usize) -> Result<bool, String> {
      let mut hooks = HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
      let Some(undo) = hooks.get_mut(id).and_then(Option::take) else {
         return Ok(false);
      };
      drop(hooks);
      SLOTS[id].limit.store(0, Ordering::Relaxed);
      // SAFETY: as for `attach`.
      #[cfg(any(windows, target_os = "linux"))]
      let undone = unsafe { undo.remove(image) };
      // SAFETY: puts back the bytes `attach` replaced. The stub and trampoline
      // stay mapped for calls already inside them.
      #[cfg(target_os = "macos")]
      let undone = unsafe { image.write_code(SLOTS[id].addr.load(Ordering::Relaxed), &undo) };
      undone.map_err(|err| err.to_string())?;
      Ok(true)
   }

   pub fn list() -> Vec<Listed> {
      let hooks = HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
      (0..MAX_HOOKS)
         .filter(|&id| hooks[id].is_some())
         .map(|id| {
            Listed {
               id,
               at: place(SLOTS[id].addr.load(Ordering::Relaxed)),
               hits: SLOTS[id].hits.load(Ordering::Relaxed),
            }
         })
         .collect()
   }

   /// Runs `record` unless this thread is recording already, since recording
   /// allocates and locks, which can reach the hooked function again.
   fn guarded(record: impl FnOnce()) {
      thread_local! {
         static BUSY: Cell<bool> = const { Cell::new(false) };
      }
      if BUSY.try_with(|busy| busy.replace(true)).unwrap_or(true) {
         return;
      }
      let _ = panic::catch_unwind(AssertUnwindSafe(record));
      let _ = BUSY.try_with(|busy| busy.set(false));
   }

   #[cfg(any(windows, target_os = "linux"))]
   extern "C" fn on_hit(id: usize, regs: &Regs) {
      guarded(|| record(id, regs.args(), regs.ret));
   }

   /// Records the call and returns where it continues.
   #[cfg(target_os = "macos")]
   extern "C" fn on_hit(regs: &[u64; OBSERVED_WORDS], id: usize) -> usize {
      guarded(|| {
         // Each caller once, so a hot function still shows the rare caller.
         static SEEN: Mutex<Vec<(usize, u64)>> = Mutex::new(Vec::new());
         let caller = regs[9];
         let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
         if seen.contains(&(id, caller)) {
            return;
         }
         seen.push((id, caller));
         drop(seen);
         record(id, [regs[0], regs[1], regs[2], regs[3]], caller);
      });
      loop {
         let trampoline = TRAMPOLINES[id].load(Ordering::Acquire);
         if trampoline != 0 {
            return trampoline;
         }
         std::hint::spin_loop();
      }
   }

   fn record(id: usize, args: [u64; 4], caller: u64) {
      let Some(slot) = SLOTS.get(id) else {
         return;
      };
      if slot.hits.fetch_add(1, Ordering::Relaxed) >= slot.limit.load(Ordering::Relaxed) {
         return;
      }
      let peek = |value: u64| {
         usize::try_from(value)
            .ok()
            .filter(|addr| (0x1_0000..0x8000_0000_0000).contains(addr))
            .and_then(|addr| Os::read_memory(addr, PEEK))
            .map(|bytes| hex(&bytes))
      };
      #[cfg(any(windows, target_os = "linux"))]
      let origin = Origin::Thread(Os::current_thread_id());
      #[cfg(target_os = "macos")]
      let origin = Origin::Stack(stack());
      phoenix_runtime::remote::record(&Trace::Native(Hit {
         hook: id,
         at: place(slot.addr.load(Ordering::Relaxed)),
         args: args.map(|arg| format!("{arg:#x}")),
         peek: args.map(peek),
         caller: usize::try_from(caller).map_or_else(|_| format!("{caller:#x}"), place),
         origin,
      }));
   }

   /// Apple's arm64 ABI keeps a frame record in every function, and the
   /// hooked one has not pushed its own yet, so the walk from here passes
   /// through `on_hit` straight into the caller's frames.
   #[cfg(target_os = "macos")]
   fn stack() -> Vec<String> {
      let mut frame: usize;
      // SAFETY: reads the frame pointer register only.
      unsafe { std::arch::asm!("mov {}, x29", out(reg) frame) };
      let mut out = Vec::new();
      for _ in 0..48 {
         let Some(record) = Os::read_memory(frame, 16) else {
            break;
         };
         let (next, ret) = record.split_at(8);
         let next = usize::from_le_bytes(next.try_into().unwrap_or_default());
         let ret = usize::from_le_bytes(ret.try_into().unwrap_or_default());
         let at = place(ret);
         if at.starts_with('+') {
            out.push(at);
         }
         if next <= frame || out.len() >= 24 {
            break;
         }
         frame = next;
      }
      out
   }
}

/// `phoenix.native.hook` where `phoenix_hook::x86::observe` has no stub.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod observe {
   use phoenix_hook::image::Image;

   pub fn hook(_image: &Image<'static>, _addr: usize, _limit: u64) -> Result<usize, String> {
      Err("phoenix.native.hook is Windows-only for now".to_owned())
   }

   #[expect(clippy::unnecessary_wraps, reason = "matches the Windows signature")]
   pub const fn unhook(_image: &Image<'static>, _id: usize) -> Result<bool, String> {
      Ok(false)
   }

   pub const fn list() -> [(); 0] {
      []
   }
}
