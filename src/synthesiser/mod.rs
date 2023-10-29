use async_trait::async_trait;
use futures_util::SinkExt;
use http::{header::InvalidHeaderValue, HeaderValue};
use speech_synthesis::{AudioChannels, AudioCodec, AudioContainer, AudioEncoding, AudioFormat, SpeechSynthesiser, UtteranceConfig};
use ssml::Serialize;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, handshake::client::Request};
use url::Url;

mod stream;
pub use self::stream::AzureCognitiveSpeechServicesSynthesisEventStream;
use super::message::AzureCognitiveSpeechServicesMessage;

#[derive(Error, Debug)]
pub enum AzureCognitiveSpeechServicesSynthesiserError {
	#[error("bad endpoint URL: {0}")]
	BadEndpointUrl(#[from] url::ParseError),
	#[error("invalid key: {0}")]
	InvalidKey(#[from] InvalidHeaderValue),
	#[error("{0}")]
	Tungstenite(#[from] tokio_tungstenite::tungstenite::Error)
}

#[derive(Clone)]
pub struct AzureCognitiveSpeechServicesSynthesiser {
	endpoint: Url,
	key: HeaderValue
}

unsafe impl Sync for AzureCognitiveSpeechServicesSynthesiser {}
unsafe impl Send for AzureCognitiveSpeechServicesSynthesiser {}

impl AzureCognitiveSpeechServicesSynthesiser {
	pub async fn new(region: impl AsRef<str>, key: impl AsRef<str>) -> Result<Self, AzureCognitiveSpeechServicesSynthesiserError> {
		Ok(Self {
			endpoint: Url::parse(&format!("wss://{}.tts.speech.microsoft.com/cognitiveservices/websocket/v1", region.as_ref()))?,
			key: HeaderValue::from_str(key.as_ref())?
		})
	}

	fn build_request(&self) -> Result<Request, AzureCognitiveSpeechServicesSynthesiserError> {
		let mut request = self.endpoint.clone().into_client_request()?;
		let headers = request.headers_mut();
		headers.append("Ocp-Apim-Subscription-Key", self.key.clone());
		Ok(request)
	}
}

#[async_trait]
impl SpeechSynthesiser for AzureCognitiveSpeechServicesSynthesiser {
	type EventStream = AzureCognitiveSpeechServicesSynthesisEventStream;

	fn negotiate_audio_format(&self, mut pref: speech_synthesis::AudioFormatPreference) -> Option<AudioFormat> {
		#[allow(unused)]
		fn optimal(input: u32, options: &[u32]) -> u32 {
			let mut closest_match = None;
			let mut closest_difference = i32::MAX;

			for &option in options {
				let difference = (input as i32 - option as i32).abs();
				if difference < closest_difference || (difference == closest_difference && option > closest_match.unwrap_or(u32::MIN)) {
					closest_match = Some(option);
					closest_difference = difference;
				}
			}

			closest_match.unwrap()
		}

		let sample_rates = pref.sample_rates.take().map(|mut f| {
			f.sort_by(|a, b| b.cmp(a));
			f
		});
		let bitrates = pref.bitrates.take().map(|mut f| {
			f.sort_by(|a, b| b.cmp(a));
			f
		});
		let channels = pref.channels.take();

		fn match_container(
			container: AudioContainer,
			sample_rates: Option<&Vec<u32>>,
			#[allow(unused)] bitrates: Option<&Vec<u16>>,
			channels: Option<&Vec<AudioChannels>>
		) -> Option<AudioFormat> {
			match container {
				AudioContainer::Raw(encoding) => {
					if channels.is_some() && !channels.unwrap().iter().any(|c| c == &AudioChannels::Mono) {
						None
					} else {
						let (name, sample_rate, channels) = sample_rates
							.map(|c| {
								for c in c.iter().copied() {
									match (c, encoding) {
										(8_000, AudioEncoding::ALaw) => return Some(("raw-8khz-8bit-mono-alaw", c, AudioChannels::Mono)),
										(8_000, AudioEncoding::MuLaw) => return Some(("raw-8khz-8bit-mono-mulaw", c, AudioChannels::Mono)),
										(8_000, AudioEncoding::Pcm) => return Some(("raw-8khz-16bit-mono-pcm", c, AudioChannels::Mono)),
										(16_000, AudioEncoding::Pcm) => return Some(("raw-16khz-16bit-mono-pcm", c, AudioChannels::Mono)),
										(22_050, AudioEncoding::Pcm) => return Some(("raw-22050hz-16bit-mono-pcm", c, AudioChannels::Mono)),
										(24_000, AudioEncoding::Pcm) => return Some(("raw-24khz-16bit-mono-pcm", c, AudioChannels::Mono)),
										(44_100, AudioEncoding::Pcm) => return Some(("raw-44100hz-16bit-mono-pcm", c, AudioChannels::Mono)),
										(48_000, AudioEncoding::Pcm) => return Some(("raw-48khz-16bit-mono-pcm", c, AudioChannels::Mono)),
										_ => continue
									};
								}
								None
							})
							.unwrap_or(Some(("raw-48khz-16bit-mono-pcm", 48_000, AudioChannels::Mono)))?;
						Some(AudioFormat::new_named(name, sample_rate, channels, None, container))
					}
				}
				AudioContainer::Ogg(AudioCodec::Opus) => {
					if channels.is_some() && !channels.unwrap().iter().any(|c| c == &AudioChannels::Mono) {
						None
					} else {
						let (name, sample_rate, channels) = sample_rates
							.map(|c| {
								for c in c.iter().copied() {
									match c {
										16_000 => return Some(("ogg-16khz-16bit-mono-opus", c, AudioChannels::Mono)),
										24_000 => return Some(("ogg-24khz-16bit-mono-opus", c, AudioChannels::Mono)),
										48_000 => return Some(("ogg-48khz-16bit-mono-opus", c, AudioChannels::Mono)),
										_ => continue
									};
								}
								None
							})
							.unwrap_or(Some(("ogg-48khz-16bit-mono-opus", 48_000, AudioChannels::Mono)))?;
						Some(AudioFormat::new_named(name, sample_rate, channels, None, container))
					}
				}
				// TODO: other formats
				_ => None
			}
		}

		if let Some(containers) = pref.containers.take() {
			for container in containers {
				if let Some(format) = match_container(container, sample_rates.as_ref(), bitrates.as_ref(), channels.as_ref()) {
					return Some(format);
				}
			}
			None
		} else {
			Some(AudioFormat::new_named("raw-48khz-16bit-mono-pcm", 48_000, AudioChannels::Mono, None, AudioContainer::Raw(AudioEncoding::Pcm)))
		}
	}

	async fn synthesise_ssml_stream(
		&self,
		input: ssml::Speak,
		audio_format: &AudioFormat,
		config: &UtteranceConfig
	) -> speech_synthesis::Result<Self::EventStream> {
		let ssml = input.serialize_to_string(ssml::Flavor::MicrosoftAzureCognitiveSpeechServices)?;
		let request = self.build_request()?;
		let addr = request.uri();
		let socket = TcpStream::connect(format!("{}:{}", addr.host().unwrap(), addr.port_u16().unwrap_or(443))).await?;
		let mut websocket = tokio_tungstenite::client_async_tls(request, socket).await?.0;
		websocket
			.send(
				AzureCognitiveSpeechServicesMessage::builder("speech.config", AzureCognitiveSpeechServicesMessage::gen_request_id())
					.with_content_type(AzureCognitiveSpeechServicesMessage::CONTENT_TYPE_JSON)
					.with_body(
						r#"{"context":{"system":{"version":"1.30.0","name":"SpeechSDK","build":"Windows-x64"},"os":{"platform":"Windows","name":"Client","version":"10"}}}"#
					)
					.build()?
					.into_websocket_message()
			)
			.await?;

		let request_id = AzureCognitiveSpeechServicesMessage::gen_request_id();

		websocket
			.send(
				AzureCognitiveSpeechServicesMessage::builder("synthesis.context", &request_id)
					.with_content_type(AzureCognitiveSpeechServicesMessage::CONTENT_TYPE_JSON)
					.with_body(format!(
						r#"{{"synthesis":{{"audio":{{"metadataOptions":{{"sentenceBoundaryEnabled":{},"wordBoundaryEnabled":{},"sessionEndEnabled":false}},"outputFormat":"{}"}}}}}}"#,
						config.emit_sentence_boundary_events,
						config.emit_word_boundary_events,
						audio_format.name().unwrap()
					))
					.build()?
					.into_websocket_message()
			)
			.await?;
		websocket
			.send(
				AzureCognitiveSpeechServicesMessage::builder("ssml", &request_id)
					.with_content_type(AzureCognitiveSpeechServicesMessage::CONTENT_TYPE_SSML)
					.with_body(ssml)
					.build()?
					.into_websocket_message()
			)
			.await?;

		Ok(AzureCognitiveSpeechServicesSynthesisEventStream::new(request_id, websocket))
	}

	async fn synthesise_text_stream(
		&self,
		text: impl AsRef<str> + Send,
		audio_format: &AudioFormat,
		config: &UtteranceConfig
	) -> speech_synthesis::Result<Self::EventStream> {
		self.synthesise_ssml_stream(
			ssml::Speak::new(
				Some("en-US"),
				[ssml::voice(
					config
						.voice
						.as_ref()
						.ok_or_else(|| anyhow::anyhow!("synthesise_text_stream requires a voice to be configured"))?,
					[text.as_ref()]
				)]
			),
			audio_format,
			config
		)
		.await
	}
}

#[cfg(test)]
mod tests {
	use speech_synthesis::*;

	use super::*;

	#[tokio::test]
	async fn test_pref() -> anyhow::Result<()> {
		let synthesiser = AzureCognitiveSpeechServicesSynthesiser::new("dummy", "dummy").await?;
		let pref = AudioFormatPreference::default()
			.with_prefer_containers([AudioContainer::Raw(AudioEncoding::Pcm), AudioContainer::Ogg(AudioCodec::Opus)])
			.with_prefer_channels([AudioChannels::Mono])
			.with_prefer_sample_rates([8_000, 16_000, 22_050, 24_000, 44_100]);
		assert_eq!(synthesiser.negotiate_audio_format(pref).unwrap().name(), Some("raw-44100hz-16bit-mono-pcm"));
		Ok(())
	}
}
