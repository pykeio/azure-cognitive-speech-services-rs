use std::env;

use azure_cognitive_speech_services::AzureCognitiveSpeechServicesSynthesiser;
use futures_util::StreamExt;
use rodio::{OutputStream, Sink, buffer::SamplesBuffer, queue::queue};
use speech_synthesis::{AudioChannels, AudioContainer, AudioEncoding, AudioFormatPreference, SpeechSynthesiser, UtteranceConfig, UtteranceEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	tracing_subscriber::fmt::init();

	let region = env::var("ACSS_REGION").expect("define ACSS_REGION");
	let key = env::var("ACSS_KEY").expect("define ACSS_KEY");

	let (_stream, stream_handle) = OutputStream::try_default().unwrap();
	let sink = Sink::try_new(&stream_handle)?;
	let (queue_input, queue_output) = queue::<i16>(false);

	let synthesiser = AzureCognitiveSpeechServicesSynthesiser::new(region, key);
	let format = synthesiser
		.negotiate_audio_format(
			&AudioFormatPreference::default()
				.with_prefer_containers([AudioContainer::Raw(AudioEncoding::PcmI16)])
				.with_prefer_channels([AudioChannels::Mono])
				.with_prefer_sample_rates([48_000])
		)
		.unwrap();

	let utterance_config = UtteranceConfig::default();
	let utterance_stream = synthesiser
		.synthesise_ssml_stream(
			&ssml::speak(Some("en-US"), [ssml::voice("en-US-JaneNeural", ["This is an example of ACSS in Rust."])]),
			&format,
			&utterance_config
		)
		.await?;
	futures_util::pin_mut!(utterance_stream);
	while let Some(event) = utterance_stream.next().await.transpose()? {
		if let UtteranceEvent::AudioChunk(audio) = event {
			queue_input.append(SamplesBuffer::new(
				1,
				48_000,
				(0..audio.len())
					.step_by(2)
					.map(|i| i16::from_le_bytes([audio[i], audio[i + 1]]))
					.collect::<Vec<i16>>()
			));
		}
	}

	sink.append(queue_output);
	sink.sleep_until_end();

	Ok(())
}
