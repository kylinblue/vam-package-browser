//! LLM tagging pipeline. Reads packages from the SQLite index, builds per-
//! package JSONL records, calls the Grok API in batches, and writes tag
//! assignments back. Bundled with the local-embedding pipeline (separate
//! milestone — `embed-library`) under the same module umbrella.

pub mod family;
pub mod grok;
pub mod prompt;
pub mod record;
pub mod runner;
pub mod seeder;
