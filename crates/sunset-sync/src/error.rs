//! Error type — see Task 2.

pub enum Error {}

pub type Result<T> = std::result::Result<T, Error>;
