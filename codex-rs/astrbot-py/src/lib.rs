//! `codex_astrbot`: in-process codex-core for AstrBot (fork addition).
//!
//! Every method is async on the Python side and exchanges JSON strings, e.g.
//!
//! ```python
//! rt = await codex_astrbot.Runtime.create(json.dumps({"codex_home": home}))
//! info = json.loads(await rt.start_thread(json.dumps({"cwd": cwd})))
//! await rt.submit_turn(info["thread_id"], json.dumps({"input": [...]}))
//! event = json.loads(await rt.next_event(info["thread_id"]))
//! ```

mod account;
mod convert;
pub mod engine;
#[cfg(feature = "python")]
mod python;
