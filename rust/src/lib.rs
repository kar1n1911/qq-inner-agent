//! 内核模块入口；供运行时模块与 Node 交叉验证使用。
pub mod activity;
pub mod config;
pub mod control;
pub mod conversation;
pub mod engine;
pub mod expression;
pub mod media;
pub mod media_select;
pub mod media_source;
pub mod memory;
pub mod onebot;
pub mod orientation;
pub mod policy;
pub mod prompts;
pub mod provider;
pub mod ranking;
pub mod sending;
pub mod settings;
pub mod store;
pub mod text;

pub mod humanize;

pub mod owner_teaching;

pub mod decision;

pub mod affect;
pub mod recall;
