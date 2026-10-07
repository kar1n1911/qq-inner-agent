//! 内核模块入口；供运行时模块与 Node 交叉验证使用。
pub mod config;
pub mod control;
pub mod engine;
pub mod media;
pub mod media_select;
pub mod media_source;
pub mod memory;
pub mod onebot;
pub mod prompts;
pub mod provider;
pub mod settings;
pub mod store;

pub mod topic_source;

pub mod relay;

pub mod persona;
