//! The parts of menus that are plain data and rules, which the main process and every shell share:
//! the roles and what they default to ([`roles`]), the order a template is put in ([`sort`]),
//! and the table that says which menu item a pressed key chooses ([`book`]).

pub mod book;
pub mod roles;
pub mod sort;

pub use book::MenuBook;
