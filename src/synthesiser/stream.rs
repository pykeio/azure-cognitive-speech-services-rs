use futures_util::{Stream, StreamExt};
use simd_json::prelude::*;
use speech_synthesis::{BlendShape, BlendShapeVisemeFrame, UtteranceEvent};
use tokio::net::TcpStream;
use tokio_websockets::{MaybeTlsStream, WebSocketStream};

use crate::{Error, message::AzureCognitiveSpeechServicesMessage};

#[rustfmt::skip]
const AZURE_BLENDSHAPE_KEYS: [&str; 55] = [
	"eyeBlinkLeft", "eyeLookDownLeft", "eyeLookInLeft", "eyeLookOutLeft", "eyeLookUpLeft", "eyeSquintLeft", "eyeWideLeft",
	"eyeBlinkRight", "eyeLookDownRight", "eyeLookInRight", "eyeLookOutRight", "eyeLookUpRight", "eyeSquintRight", "eyeWideRight",
	"jawForward", "jawLeft", "jawRight", "jawOpen", "mouthClose", "mouthFunnel", "mouthPucker", "mouthLeft", "mouthRight",
	"mouthSmileLeft", "mouthSmileRight", "mouthFrownLeft", "mouthFrownRight", "mouthDimpleLeft", "mouthDimpleRight",
	"mouthStretchLeft", "mouthStretchRight", "mouthRollLower", "mouthRollUpper", "mouthShrugLower", "mouthShrugUpper",
	"mouthPressLeft", "mouthPressRight", "mouthLowerDownLeft", "mouthLowerDownRight", "mouthUpperUpLeft", "mouthUpperUpRight",
	"browDownLeft", "browDownRight", "browInnerUp", "browOuterUpLeft", "browOuterUpRight", "cheekPuff", "cheekSquintLeft",
	"cheekSquintRight", "noseSneerLeft", "noseSneerRight", "tongueOut", "headRoll", "leftEyeRoll", "rightEyeRoll"
];

pub fn stream(
	request_id: impl ToString,
	mut websocket: WebSocketStream<MaybeTlsStream<TcpStream>>
) -> impl Stream<Item = crate::Result<UtteranceEvent>> + Send + 'static {
	let request_id = request_id.to_string();

	async_stream_lite::try_async_stream(|yielder| async move {
		let mut self_stream_id = None;
		while let Some(msg) = websocket.next().await {
			let msg = msg?;
			let msg: AzureCognitiveSpeechServicesMessage = if msg.is_binary() {
				(&*msg.into_payload()).try_into()?
			} else if msg.is_text() {
				msg.as_text().unwrap().parse()?
			} else if msg.is_close() {
				tracing::error!("received unexpected close frame: {:?}", msg.as_close());
				break;
			} else {
				continue;
			};

			debug_assert_eq!(msg.request_id(), request_id);

			match msg.path() {
				"turn.start" => continue,
				"turn.end" => break,
				"audio" => {
					yielder
						.r#yield(UtteranceEvent::AudioChunk(msg.into_body().into_binary().ok_or(Error::ExpectedBinary("audio"))?))
						.await
				}
				"audio.metadata" => {
					let data = msg.into_json_abstract()?;
					let metadata = &data
						.get_array("Metadata")
						.ok_or(Error::MissingField("Metadata", "`audio.metadata` event"))?[0];
					let meta_type = metadata.get_str("Type").ok_or(Error::MissingField("Type", "`audio.metadata` event"))?;
					let metadata = metadata.get("Data").ok_or(Error::MissingField("Data", "`audio.metadata` event"))?;

					let is_boundary = meta_type == "WordBoundary" || meta_type == "SentenceBoundary";

					let (from_millis, to_millis, text) = if is_boundary {
						// timestamps are given in "ticks", we need to divide by 10,000 to get milliseconds
						let from_millis = metadata
							.get_u64("Offset")
							.map(|o| o as f32 / 10_000.)
							.ok_or(Error::MissingField("Offset", "`audio.metadata` event"))?;
						let to_millis = from_millis
							+ metadata
								.get_u64("Duration")
								.map(|o| o as f32 / 10_000.)
								.ok_or(Error::MissingField("Duration", "`audio.metadata` event"))?;
						let text = metadata
							.get("text")
							.and_then(|v| v.get_str("Text"))
							.ok_or(Error::MissingField("Text", "`audio.metadata` event"))?
							.to_owned();
						(Some(from_millis), Some(to_millis), Some(text))
					} else {
						(None, None, None)
					};

					yielder
						.r#yield(match meta_type {
							"SentenceBoundary" => UtteranceEvent::SentenceBoundary {
								from_millis: from_millis.unwrap(),
								to_millis: to_millis.unwrap(),
								text: text.unwrap().into_boxed_str()
							},
							"WordBoundary" => UtteranceEvent::WordBoundary {
								from_millis: from_millis.unwrap(),
								to_millis: to_millis.unwrap(),
								text: text.unwrap().into_boxed_str()
							},
							"Viseme" => {
								// ACSS sends blendshape frames at 60 fps.
								const FRAME_TICK: f32 = 1000. / 60.;

								#[derive(serde::Deserialize)]
								struct AnimationChunk {
									#[serde(rename = "FrameIndex")]
									frame_index: usize,
									#[serde(rename = "BlendShapes")]
									blend_shapes: Vec<Vec<f32>>
								}
								let mut chunk = metadata
									.get_str("AnimationChunk")
									.ok_or(Error::MissingField("AnimationChunk", "`audio.metadata` event"))?
									.to_string();
								let animation_chunk: AnimationChunk = unsafe { simd_json::from_str(&mut chunk) }?;

								let offset_ms = animation_chunk.frame_index as f32 * FRAME_TICK;
								UtteranceEvent::BlendShapeVisemesChunk(
									animation_chunk
										.blend_shapes
										.into_iter()
										.enumerate()
										.map(|(i, keys)| BlendShapeVisemeFrame {
											frame_offset: offset_ms + (i as f32 * FRAME_TICK),
											blendshapes: keys
												.into_iter()
												.enumerate()
												.map(|(i, weight)| BlendShape {
													key: AZURE_BLENDSHAPE_KEYS[i].into(),
													weight
												})
												.collect()
										})
										.collect()
								)
							}
							a => unimplemented!("{a}")
						})
						.await;
				}
				"response" => {
					let data = msg.into_json_abstract()?;
					let audio = data.get("audio").ok_or(Error::MissingField("audio", "`response` event"))?;
					let audio_type = audio
						.get_str("type")
						.ok_or(Error::MissingField("type", "`response` event audio metadata"))?;
					debug_assert_eq!(audio_type, "inline");

					// we shouldn't be receiving multiple streams in one request
					let stream_id = audio
						.get_str("streamId")
						.ok_or(Error::MissingField("streamId", "`response` event audio metadata"))?;
					if let Some(self_stream_id) = &self_stream_id {
						if self_stream_id != stream_id {
							return Err(Error::UnexpectedMultipleStreams)?;
						}
					} else {
						self_stream_id = Some(stream_id.to_owned());
					}
				}
				t => {
					tracing::error!("unhandled event {t}");
				}
			}
		}
		Ok(())
	})
}
