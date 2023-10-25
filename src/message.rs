use std::{
	collections::HashMap,
	fmt::{Debug, Write},
	str::{FromStr, Utf8Error}
};

use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

#[derive(Error, Debug)]
pub enum AzureCognitiveSpeechServicesMessageError {
	#[error("Cannot parse a binary message into JSON")]
	ParseBinary,
	#[error("Failed to parse JSON message: {0}")]
	ParseJson(#[from] simd_json::Error),
	#[error("Couldn't build Azure Cognitive Speech Services message: {0}")]
	Builder(&'static str),
	#[error("Bad header format: `{0}`")]
	BadHeader(String),
	#[error("Missing required header '{0}'")]
	MissingHeader(&'static str),
	#[error("Malformed message header section")]
	MalformedHeaderSection,
	#[error("Invalid UTF-8 in binary message header section: {0}")]
	HeaderUtf8(#[from] Utf8Error)
}

#[derive(Clone, PartialEq, Eq)]
pub enum AzureCognitiveSpeechServicesMessageBody {
	Binary(Box<[u8]>),
	Text(String)
}

impl AzureCognitiveSpeechServicesMessageBody {
	pub fn as_binary(&self) -> Option<&[u8]> {
		match self {
			AzureCognitiveSpeechServicesMessageBody::Binary(b) => Some(b.as_ref()),
			AzureCognitiveSpeechServicesMessageBody::Text(_) => None
		}
	}

	pub fn as_text(&self) -> Option<&String> {
		match self {
			AzureCognitiveSpeechServicesMessageBody::Binary(_) => None,
			AzureCognitiveSpeechServicesMessageBody::Text(t) => Some(t)
		}
	}

	pub fn into_binary(self) -> Option<Box<[u8]>> {
		match self {
			AzureCognitiveSpeechServicesMessageBody::Binary(b) => Some(b),
			AzureCognitiveSpeechServicesMessageBody::Text(_) => None
		}
	}

	pub fn into_text(self) -> Option<String> {
		match self {
			AzureCognitiveSpeechServicesMessageBody::Binary(_) => None,
			AzureCognitiveSpeechServicesMessageBody::Text(t) => Some(t)
		}
	}
}

impl Debug for AzureCognitiveSpeechServicesMessageBody {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Text(text) => {
				f.write_char('"')?;
				f.write_str(text)?;
				f.write_char('"')
			}
			Self::Binary(_) => f.write_str("<binary>")
		}
	}
}

impl From<Vec<u8>> for AzureCognitiveSpeechServicesMessageBody {
	fn from(value: Vec<u8>) -> Self {
		Self::Binary(value.into_boxed_slice())
	}
}

impl From<String> for AzureCognitiveSpeechServicesMessageBody {
	fn from(value: String) -> Self {
		Self::Text(value)
	}
}

impl<'s> From<&'s str> for AzureCognitiveSpeechServicesMessageBody {
	fn from(value: &'s str) -> Self {
		Self::Text(value.to_owned())
	}
}

#[derive(Debug, Clone)]
pub struct AzureCognitiveSpeechServicesMessage {
	request_id: String,
	path: String,
	content_type: Option<String>,
	stream_id: Option<String>,
	body: AzureCognitiveSpeechServicesMessageBody
}

impl AzureCognitiveSpeechServicesMessage {
	pub const CONTENT_TYPE_JSON: &'static str = "application/json";
	pub const CONTENT_TYPE_SSML: &'static str = "application/ssml+xml";

	pub fn builder(path: impl ToString, request_id: impl ToString) -> AzureCognitiveSpeechServicesMessageBuilder {
		AzureCognitiveSpeechServicesMessageBuilder::new(path, request_id)
	}

	pub fn gen_request_id() -> String {
		Uuid::new_v4().simple().to_string()
	}

	pub fn path(&self) -> &str {
		&self.path
	}

	pub fn request_id(&self) -> &str {
		&self.request_id
	}

	pub fn content_type(&self) -> Option<&String> {
		self.content_type.as_ref()
	}

	pub fn stream_id(&self) -> Option<&String> {
		self.stream_id.as_ref()
	}

	pub fn body(&self) -> &AzureCognitiveSpeechServicesMessageBody {
		&self.body
	}

	pub fn into_body(self) -> AzureCognitiveSpeechServicesMessageBody {
		self.body
	}

	pub fn serialize_text(self) -> String {
		let (headers, body) = self.serialize_inner();
		if let AzureCognitiveSpeechServicesMessageBody::Text(body) = body {
			format!("{headers}\r\n\r\n{body}")
		} else {
			panic!("expected text");
		}
	}

	pub fn serialize_binary(self) -> Vec<u8> {
		let (headers, body) = self.serialize_inner();
		if let AzureCognitiveSpeechServicesMessageBody::Binary(body) = body {
			[&u16::to_be_bytes(headers.len() as u16), headers.as_bytes(), &body].concat()
		} else {
			panic!("expected binary");
		}
	}

	fn serialize_inner(self) -> (String, AzureCognitiveSpeechServicesMessageBody) {
		let mut headers = HashMap::new();
		headers.insert("X-RequestId", self.request_id);
		if let Some(content_type) = self.content_type {
			headers.insert("Content-Type", content_type);
		}
		if let Some(stream_id) = self.stream_id {
			headers.insert("X-StreamId", stream_id);
		}
		headers.insert("Path", self.path);
		let headers = headers
			.into_iter()
			.map(|(name, value)| format!("{name}: {value}"))
			.collect::<Vec<_>>()
			.join("\r\n");
		(headers, self.body)
	}

	pub fn into_websocket_message(self) -> Message {
		if matches!(&self.body, AzureCognitiveSpeechServicesMessageBody::Binary(_)) {
			Message::Binary(self.serialize_binary())
		} else {
			Message::Text(self.serialize_text())
		}
	}

	pub fn into_json<D: DeserializeOwned>(self) -> Result<D, AzureCognitiveSpeechServicesMessageError> {
		let mut body = self
			.into_body()
			.into_text()
			.ok_or(AzureCognitiveSpeechServicesMessageError::ParseBinary)?;
		Ok(simd_json::from_slice(unsafe { body.as_bytes_mut() })?)
	}

	pub fn into_json_abstract(self) -> Result<simd_json::OwnedValue, AzureCognitiveSpeechServicesMessageError> {
		let mut body = self
			.into_body()
			.into_text()
			.ok_or(AzureCognitiveSpeechServicesMessageError::ParseBinary)?;
		Ok(simd_json::to_owned_value(unsafe { body.as_bytes_mut() })?)
	}
}

#[derive(Default, Clone)]
pub struct AzureCognitiveSpeechServicesMessageBuilder {
	request_id: Option<String>,
	path: Option<String>,
	content_type: Option<String>,
	stream_id: Option<String>,
	body: Option<AzureCognitiveSpeechServicesMessageBody>
}

impl AzureCognitiveSpeechServicesMessageBuilder {
	pub fn new(path: impl ToString, request_id: impl ToString) -> Self {
		Self {
			path: Some(path.to_string()),
			request_id: Some(request_id.to_string()),
			..Default::default()
		}
	}

	pub fn with_content_type(mut self, content_type: impl ToString) -> Self {
		self.content_type = Some(content_type.to_string());
		self
	}

	pub fn with_stream_id(mut self, stream_id: impl ToString) -> Self {
		self.stream_id = Some(stream_id.to_string());
		self
	}

	pub fn with_body(mut self, body: impl Into<AzureCognitiveSpeechServicesMessageBody>) -> Self {
		self.body = Some(body.into());
		self
	}

	pub fn build(self) -> Result<AzureCognitiveSpeechServicesMessage, AzureCognitiveSpeechServicesMessageError> {
		Ok(AzureCognitiveSpeechServicesMessage {
			request_id: self
				.request_id
				.ok_or(AzureCognitiveSpeechServicesMessageError::Builder("missing request ID"))?,
			path: self
				.path
				.ok_or(AzureCognitiveSpeechServicesMessageError::Builder("missing request path"))?,
			content_type: self.content_type,
			stream_id: self.stream_id,
			body: self
				.body
				.ok_or(AzureCognitiveSpeechServicesMessageError::Builder("missing message body"))?
		})
	}
}

fn parse_headers(headers: impl AsRef<str>) -> Result<AzureCognitiveSpeechServicesMessageBuilder, AzureCognitiveSpeechServicesMessageError> {
	let mut headers = headers
		.as_ref()
		.split("\r\n")
		.filter(|c| !c.trim().is_empty())
		.map(|c| {
			let (header_name, header_value) = c
				.split_once(':')
				.ok_or_else(|| AzureCognitiveSpeechServicesMessageError::BadHeader(c.to_string()))?;
			Ok((header_name.trim().to_lowercase(), header_value.trim().to_lowercase()))
		})
		.collect::<Result<HashMap<_, _>, AzureCognitiveSpeechServicesMessageError>>()?;

	let mut builder = AzureCognitiveSpeechServicesMessageBuilder::new(
		headers
			.remove("path")
			.ok_or(AzureCognitiveSpeechServicesMessageError::MissingHeader("Path"))?,
		headers
			.remove("x-requestid")
			.ok_or(AzureCognitiveSpeechServicesMessageError::MissingHeader("X-RequestId"))?
	);
	if let Some(content_type) = headers.remove("content-type") {
		builder = builder.with_content_type(content_type);
	}
	if let Some(stream_id) = headers.remove("x-streamid") {
		builder = builder.with_stream_id(stream_id);
	}
	Ok(builder)
}

impl FromStr for AzureCognitiveSpeechServicesMessage {
	type Err = AzureCognitiveSpeechServicesMessageError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let (headers, body) = s
			.split_once("\r\n\r\n")
			.ok_or(AzureCognitiveSpeechServicesMessageError::MalformedHeaderSection)?;
		parse_headers(headers)?
			.with_body(AzureCognitiveSpeechServicesMessageBody::Text(body.to_string()))
			.build()
	}
}

impl TryFrom<Vec<u8>> for AzureCognitiveSpeechServicesMessage {
	type Error = AzureCognitiveSpeechServicesMessageError;

	fn try_from(value: Vec<u8>) -> Result<Self, AzureCognitiveSpeechServicesMessageError> {
		let (int_bytes, rest) = value.split_at(std::mem::size_of::<u16>());
		let header_len = u16::from_be_bytes([int_bytes[0], int_bytes[1]]) as usize;
		let headers = std::str::from_utf8(&rest[..header_len])?;
		let body = &rest[header_len..];
		parse_headers(headers)?
			.with_body(AzureCognitiveSpeechServicesMessageBody::Binary(body.to_vec().into_boxed_slice()))
			.build()
	}
}
