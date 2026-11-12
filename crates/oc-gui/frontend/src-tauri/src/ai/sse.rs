//! Incremental server-sent-events decoder shared by every native adapter.
use super::{LIMIT, ProviderError, ProviderErrorKind};

/// One decoded server-sent event.
#[derive(Debug)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental SSE frame decoder.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<u8>,
}

fn protocol(message: &str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Protocol, message)
}

impl SseDecoder {
    /// Feed a chunk and dispatch every complete frame.
    pub fn push<F>(&mut self, chunk: &[u8], on_frame: &mut F) -> Result<(), ProviderError>
    where
        F: FnMut(SseFrame) -> Result<(), ProviderError>,
    {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > LIMIT {
            return Err(protocol("Provider response exceeds 2 MiB limit"));
        }
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=end).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.line(&line, on_frame)?;
        }
        Ok(())
    }

    /// Dispatch any trailing frame.
    pub fn finish<F>(&mut self, on_frame: &mut F) -> Result<(), ProviderError>
    where
        F: FnMut(SseFrame) -> Result<(), ProviderError>,
    {
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.line(&line, on_frame)?;
        }
        self.dispatch(on_frame)
    }

    fn line<F>(&mut self, line: &[u8], on_frame: &mut F) -> Result<(), ProviderError>
    where
        F: FnMut(SseFrame) -> Result<(), ProviderError>,
    {
        if line.is_empty() {
            return self.dispatch(on_frame);
        }
        if line[0] == b':' {
            return Ok(());
        }
        let (field, value) = match line.iter().position(|byte| *byte == b':') {
            Some(index) => (&line[..index], &line[index + 1..]),
            None => (line, &[][..]),
        };
        let value = value.strip_prefix(b" ").unwrap_or(value);
        match field {
            b"event" => {
                self.event = Some(
                    std::str::from_utf8(value)
                        .map_err(|_| protocol("Invalid streaming UTF-8"))?
                        .to_owned(),
                );
            }
            b"data" => {
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(value);
            }
            _ => {}
        }
        Ok(())
    }

    fn dispatch<F>(&mut self, on_frame: &mut F) -> Result<(), ProviderError>
    where
        F: FnMut(SseFrame) -> Result<(), ProviderError>,
    {
        let event = self.event.take();
        if self.data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.data);
        let data = String::from_utf8(data).map_err(|_| protocol("Invalid streaming UTF-8"))?;
        on_frame(SseFrame { event, data })
    }
}

/// Drive a response body through the decoder.
pub async fn drive<F>(
    response: &mut reqwest::Response,
    on_frame: &mut F,
) -> Result<(), ProviderError>
where
    F: FnMut(SseFrame) -> Result<(), ProviderError>,
{
    let mut decoder = SseDecoder::default();
    let mut total = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| super::stream_read_error(error.is_timeout()))?
    {
        total += chunk.len();
        if total > super::RESPONSE_BYTES_LIMIT {
            return Err(protocol(&format!(
                "Provider response exceeds {} MiB limit",
                super::RESPONSE_BYTES_LIMIT / (1024 * 1024)
            )));
        }
        decoder.push(&chunk, on_frame)?;
    }
    decoder.finish(on_frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_named_events_comments_and_split_chunks() {
        let mut decoder = SseDecoder::default();
        let mut frames = Vec::new();
        let mut collect = |frame: SseFrame| {
            frames.push((frame.event, frame.data));
            Ok(())
        };
        let stream = ": keepalive\r\nevent: alpha\r\ndata: one\r\ndata: two\r\n\r\n\
                      data: three\n\n";
        for chunk in stream.as_bytes().chunks(3) {
            decoder.push(chunk, &mut collect).unwrap();
        }
        decoder.finish(&mut collect).unwrap();
        assert_eq!(
            frames,
            vec![
                (Some("alpha".into()), "one\ntwo".into()),
                (None, "three".into())
            ]
        );
    }

    #[test]
    fn rejects_invalid_utf8_and_oversize_buffers() {
        let mut decoder = SseDecoder::default();
        let mut ignore = |_: SseFrame| Ok(());
        assert!(decoder.push(b"data: \xff\n\n", &mut ignore).is_err());
        let mut decoder = SseDecoder::default();
        let oversized = vec![b'a'; LIMIT + 1];
        assert!(decoder.push(&oversized, &mut ignore).is_err());
    }
}
