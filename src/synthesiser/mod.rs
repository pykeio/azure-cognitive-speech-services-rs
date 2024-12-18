use futures_util::{SinkExt, Stream};
use http::{HeaderName, HeaderValue};
use speech_synthesis::{AudioChannels, AudioCodec, AudioContainer, AudioEncoding, AudioFormat, SpeechSynthesiser, UtteranceConfig, UtteranceEvent};
use ssml::{Serialize, SerializeOptions};
use tokio_websockets::ClientBuilder;

mod stream;
use super::message::AzureCognitiveSpeechServicesMessage;
use crate::Error;

#[derive(Clone)]
pub struct AzureCognitiveSpeechServicesSynthesiser {
	endpoint: String,
	key: HeaderValue
}

unsafe impl Sync for AzureCognitiveSpeechServicesSynthesiser {}
unsafe impl Send for AzureCognitiveSpeechServicesSynthesiser {}

impl AzureCognitiveSpeechServicesSynthesiser {
	pub async fn new(region: impl AsRef<str>, key: impl AsRef<str>) -> crate::Result<Self> {
		Ok(Self {
			endpoint: format!("wss://{}.tts.speech.microsoft.com/cognitiveservices/websocket/v1", region.as_ref()),
			key: HeaderValue::from_str(key.as_ref())?
		})
	}

	fn build_client(&self) -> crate::Result<ClientBuilder> {
		Ok(ClientBuilder::new()
			.uri(self.endpoint.as_str())
			.unwrap()
			.add_header(HeaderName::from_static("ocp-apim-subscription-key"), self.key.clone()))
	}

	fn name_for_format(format: &AudioFormat) -> Option<&str> {
		match (format.container(), format.sample_rate(), format.channels()) {
			(AudioContainer::Raw(AudioEncoding::ALaw), 8000, AudioChannels::Mono) => Some("raw-8khz-8bit-mono-alaw"),
			(AudioContainer::Raw(AudioEncoding::MuLaw), 8000, AudioChannels::Mono) => Some("raw-8khz-8bit-mono-mulaw"),
			(AudioContainer::Raw(AudioEncoding::PcmI16), 8000, AudioChannels::Mono) => Some("raw-8khz-16bit-mono-pcm"),
			(AudioContainer::Raw(AudioEncoding::PcmI16), 16000, AudioChannels::Mono) => Some("raw-16khz-16bit-mono-pcm"),
			(AudioContainer::Raw(AudioEncoding::PcmI16), 22050, AudioChannels::Mono) => Some("raw-22050khz-16bit-mono-pcm"),
			(AudioContainer::Raw(AudioEncoding::PcmI16), 24000, AudioChannels::Mono) => Some("raw-24khz-16bit-mono-pcm"),
			(AudioContainer::Raw(AudioEncoding::PcmI16), 44100, AudioChannels::Mono) => Some("raw-44100khz-16bit-mono-pcm"),
			(AudioContainer::Raw(AudioEncoding::PcmI16), 48000, AudioChannels::Mono) => Some("raw-48khz-16bit-mono-pcm"),
			(AudioContainer::Ogg(AudioCodec::Opus), 16000, AudioChannels::Mono) => Some("ogg-16khz-16bit-mono-opus"),
			(AudioContainer::Ogg(AudioCodec::Opus), 24000, AudioChannels::Mono) => Some("ogg-24khz-16bit-mono-opus"),
			(AudioContainer::Ogg(AudioCodec::Opus), 48000, AudioChannels::Mono) => Some("ogg-48khz-16bit-mono-opus"),
			_ => None
		}
	}

	async fn speak_inner(
		&self,
		ssml_string: String,
		audio_format: &AudioFormat,
		config: &UtteranceConfig
	) -> crate::Result<impl Stream<Item = crate::Result<UtteranceEvent>> + Send + 'static> {
		let client_builder = self.build_client()?;
		let (mut websocket, _response) = client_builder.connect().await?;
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
						r#"{{"synthesis":{{"audio":{{"metadataOptions":{{"sentenceBoundaryEnabled":{},"wordBoundaryEnabled":{},"bookmarkEnabled":true,"sessionEndEnabled":false}},"outputFormat":"{}"}}}}}}"#,
						config.emit_sentence_boundary_events,
						config.emit_word_boundary_events,
						Self::name_for_format(audio_format).ok_or(Error::UnsupportedAudioFormat)?
					))
					.build()?
					.into_websocket_message()
			)
			.await?;
		websocket
			.send(
				AzureCognitiveSpeechServicesMessage::builder("ssml", &request_id)
					.with_content_type(AzureCognitiveSpeechServicesMessage::CONTENT_TYPE_SSML)
					.with_body(ssml_string)
					.build()?
					.into_websocket_message()
			)
			.await?;

		Ok(self::stream::stream(request_id, websocket))
	}
}

impl SpeechSynthesiser for AzureCognitiveSpeechServicesSynthesiser {
	type Error = crate::Error;

	fn negotiate_audio_format(&self, pref: &speech_synthesis::AudioFormatPreference) -> Option<AudioFormat> {
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

		let sample_rates = pref.sample_rates.clone().map(|mut f| {
			f.sort_by(|a, b| b.cmp(a));
			f
		});
		let bitrates = pref.bitrates.clone().map(|mut f| {
			f.sort_by(|a, b| b.cmp(a));
			f
		});
		let channels = pref.channels.clone();

		fn match_container(
			container: &AudioContainer,
			sample_rates: Option<&Vec<u32>>,
			#[allow(unused)] bitrates: Option<&Vec<u16>>,
			channels: Option<&Vec<AudioChannels>>
		) -> Option<AudioFormat> {
			match container {
				AudioContainer::Raw(encoding) => {
					if channels.is_some() && !channels.unwrap().iter().any(|c| c == &AudioChannels::Mono) {
						None
					} else {
						let sample_rate = sample_rates
							.map(|sr_prefs| {
								for sr in sr_prefs.iter().copied() {
									match (sr, encoding) {
										(8_000, AudioEncoding::ALaw) => return Some(sr),
										(8_000, AudioEncoding::MuLaw) => return Some(sr),
										(8_000, AudioEncoding::PcmI16) => return Some(sr),
										(16_000, AudioEncoding::PcmI16) => return Some(sr),
										(22_050, AudioEncoding::PcmI16) => return Some(sr),
										(24_000, AudioEncoding::PcmI16) => return Some(sr),
										(44_100, AudioEncoding::PcmI16) => return Some(sr),
										(48_000, AudioEncoding::PcmI16) => return Some(sr),
										_ => continue
									};
								}
								None
							})
							.unwrap_or(Some(48_000))?;
						Some(AudioFormat::new(sample_rate, AudioChannels::Mono, None, *container))
					}
				}
				AudioContainer::Ogg(AudioCodec::Opus) => {
					if channels.is_some() && !channels.unwrap().iter().any(|c| c == &AudioChannels::Mono) {
						None
					} else {
						let sample_rate = sample_rates
							.map(|sr_prefs| {
								for sr in sr_prefs.iter().copied() {
									match sr {
										16_000 | 24_000 | 48_000 => return Some(sr),
										_ => continue
									};
								}
								None
							})
							.unwrap_or(Some(48_000))?;
						Some(AudioFormat::new(sample_rate, AudioChannels::Mono, None, *container))
					}
				}
				// TODO: other formats
				_ => None
			}
		}

		if let Some(containers) = pref.containers.as_ref() {
			for container in containers {
				if let Some(format) = match_container(container, sample_rates.as_ref(), bitrates.as_ref(), channels.as_ref()) {
					return Some(format);
				}
			}
			None
		} else {
			Some(AudioFormat::new(48_000, AudioChannels::Mono, None, AudioContainer::Raw(AudioEncoding::PcmI16)))
		}
	}

	async fn synthesise_ssml_stream(
		&self,
		input: &ssml::Speak<'_>,
		audio_format: &AudioFormat,
		config: &UtteranceConfig
	) -> Result<impl Stream<Item = crate::Result<UtteranceEvent>> + Send + 'static, Self::Error> {
		let ssml_string = input.serialize_to_string(&SerializeOptions::default().flavor(ssml::Flavor::MicrosoftAzureCognitiveSpeechServices))?;
		self.speak_inner(ssml_string, audio_format, config).await
	}

	async fn synthesise_text_stream(
		&self,
		input: &str,
		audio_format: &AudioFormat,
		config: &UtteranceConfig
	) -> Result<impl speech_synthesis::UtteranceEventStream<Self::Error> + 'static, Self::Error> {
		let ssml_string = ssml::Speak::new::<ssml::Element, _>(config.voice.as_deref(), [match config.voice.as_ref() {
			Some(voice) => ssml::voice(voice.as_ref(), [input.to_string()]).into(),
			None => input.to_string().into()
		}])
		.serialize_to_string(&SerializeOptions::default().flavor(ssml::Flavor::MicrosoftAzureCognitiveSpeechServices))?;
		self.speak_inner(ssml_string, audio_format, config).await
	}
}

#[cfg(test)]
mod tests {
	use speech_synthesis::*;

	use super::*;

	#[tokio::test]
	async fn test_pref() -> crate::Result<()> {
		let synthesiser = AzureCognitiveSpeechServicesSynthesiser::new("dummy", "dummy").await?;
		let pref = AudioFormatPreference::default()
			.with_prefer_containers([AudioContainer::Raw(AudioEncoding::PcmI16), AudioContainer::Ogg(AudioCodec::Opus)])
			.with_prefer_channels([AudioChannels::Mono])
			.with_prefer_sample_rates([8_000, 16_000, 22_050, 24_000, 44_100]);
		let negotiated_format = synthesiser.negotiate_audio_format(&pref).unwrap();
		assert_eq!(negotiated_format.container(), AudioContainer::Raw(AudioEncoding::PcmI16));
		assert_eq!(negotiated_format.sample_rate(), 44100);
		assert_eq!(negotiated_format.channels(), AudioChannels::Mono);
		Ok(())
	}
}
