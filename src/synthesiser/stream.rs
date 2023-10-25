use std::{
	pin::Pin,
	task::{Context, Poll}
};

use futures_util::{Stream, StreamExt};
use simd_json::ValueAccess;
use speech_synthesis::{BlendShape, BlendShapeVisemeFrame, UtteranceEvent, UtteranceEventStream};
use tokio::net::TcpStream;
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

use crate::message::AzureCognitiveSpeechServicesMessage;

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

pub struct AzureCognitiveSpeechServicesSynthesisEventStream {
	request_id: String,
	stream_id: Option<String>,
	websocket: WebSocketStream<MaybeTlsStream<TcpStream>>
}

impl AzureCognitiveSpeechServicesSynthesisEventStream {
	pub fn new(request_id: impl ToString, websocket: WebSocketStream<MaybeTlsStream<TcpStream>>) -> Self {
		Self {
			request_id: request_id.to_string(),
			stream_id: None,
			websocket
		}
	}
}

impl Stream for AzureCognitiveSpeechServicesSynthesisEventStream {
	type Item = speech_synthesis::Result<UtteranceEvent>;

	#[tracing::instrument(skip_all)]
	fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
		let msg = self.websocket.poll_next_unpin(cx);
		match msg {
			Poll::Ready(Some(msg)) => {
				let msg: AzureCognitiveSpeechServicesMessage = match msg? {
					Message::Binary(data) => data.try_into()?,
					Message::Text(text) => text.parse()?,
					Message::Close(e) => {
						tracing::error!("received unexpected close frame: {e:?}");
						return Poll::Ready(None);
					}
					_ => {
						cx.waker().wake_by_ref();
						return Poll::Pending;
					}
				};

				debug_assert_eq!(msg.request_id(), self.request_id);

				match msg.path() {
					"turn.start" => {
						// wake so we can receive the call to process the next message
						cx.waker().wake_by_ref();
						Poll::Pending
					}
					"turn.end" => {
						// we can just end the stream here, the server will close the socket itself
						Poll::Ready(None)
					}
					"audio" => Poll::Ready(Some(Ok(UtteranceEvent::AudioChunk(
						msg.into_body()
							.into_binary()
							.ok_or(anyhow::anyhow!("expected `audio` event to have a binary body"))?
					)))),
					"audio.metadata" => {
						let data = msg.into_json_abstract()?;
						let metadata = &data
							.get_array("Metadata")
							.ok_or(anyhow::anyhow!("missing `Metadata` field in `audio.metadata` event"))?[0];
						let meta_type = metadata
							.get_str("Type")
							.ok_or(anyhow::anyhow!("missing `Type` field in `audio.metadata` event"))?;
						let metadata = metadata
							.get("Data")
							.ok_or(anyhow::anyhow!("missing `Data` field in `audio.metadata` event"))?;

						let is_boundary = meta_type == "WordBoundary" || meta_type == "SentenceBoundary";

						let (from_millis, to_millis, text) = if is_boundary {
							// timestamps are given in "ticks", we need to divide by 10,000 to get milliseconds
							let from_millis = metadata
								.get_u64("Offset")
								.map(|o| o as f32 / 10_000.)
								.ok_or(anyhow::anyhow!("missing `Offset` field in `audio.metadata` event"))?;
							let to_millis = from_millis
								+ metadata
									.get_u64("Duration")
									.map(|o| o as f32 / 10_000.)
									.ok_or(anyhow::anyhow!("missing `Duration` field in `audio.metadata` event"))?;
							let text = metadata
								.get("text")
								.and_then(|v| v.get_str("Text"))
								.ok_or(anyhow::anyhow!("missing `Text` field in `audio.metadata` event"))?
								.to_owned();
							(Some(from_millis), Some(to_millis), Some(text))
						} else {
							(None, None, None)
						};

						Poll::Ready(Some(Ok(match meta_type {
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
									.ok_or(anyhow::anyhow!("missing `AnimationChunk` field in `audio.metadata` event"))?
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
						})))
					}
					"response" => {
						let data = msg.into_json_abstract()?;
						let audio = data.get("audio").ok_or(anyhow::anyhow!("missing `audio` field in `response` event"))?;
						debug_assert_eq!(
							audio
								.get_str("type")
								.ok_or(anyhow::anyhow!("missing `type` field in `response` event audio metadata"))?,
							"inline"
						);

						// we shouldn't be receiving multiple streams in one request
						let stream_id = audio
							.get_str("streamId")
							.ok_or(anyhow::anyhow!("missing `streamId` field in `response` event audio metadata"))?;
						if let Some(self_stream_id) = &self.stream_id {
							if self_stream_id != stream_id {
								return Poll::Ready(Some(Err(anyhow::anyhow!("unexpected multiple streams in request"))));
							}
						} else {
							self.stream_id = Some(stream_id.to_owned());
						}

						// wake so we can receive the call to process the next message
						cx.waker().wake_by_ref();

						Poll::Pending
					}
					t => {
						tracing::error!("unhandled event {t}");
						cx.waker().wake_by_ref();
						Poll::Pending
					}
				}
			}
			Poll::Ready(None) => Poll::Ready(None),
			Poll::Pending => Poll::Pending
		}
	}
}

impl UtteranceEventStream for AzureCognitiveSpeechServicesSynthesisEventStream {}
