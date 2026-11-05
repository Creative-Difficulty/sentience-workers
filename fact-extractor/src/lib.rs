//! fact-extractor: extracts durable facts, activities and skills about message
//! authors with an LLM and stores them in unidb.

pub mod config;
pub mod llm;
pub mod prompt;
pub mod records;
pub mod worker;
