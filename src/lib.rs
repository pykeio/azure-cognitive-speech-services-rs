mod error;
pub mod message;
mod synthesiser;

pub use self::{
	error::{Error, Result},
	synthesiser::AzureCognitiveSpeechServicesSynthesiser
};
