use embassy_time::{Duration, with_timeout};
use embedded_io_async::{Error as _, ErrorKind, ErrorType, Read, Write};

/// Bound each write/flush call. After any error, discard the connection and transport.
/// Reads remain unbounded so MQTT can manage idle keepalive.
pub struct WriteTimeout<IO> {
    pub io: IO,
    pub timeout: Duration,
}

impl<IO: ErrorType> ErrorType for WriteTimeout<IO> {
    type Error = ErrorKind;
}

impl<IO: Read> Read for WriteTimeout<IO> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.io.read(buffer).await.map_err(|error| error.kind())
    }
}

impl<IO: Write> Write for WriteTimeout<IO> {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        with_timeout(self.timeout, self.io.write(buffer))
            .await
            .map_err(|_| ErrorKind::TimedOut)?
            .map_err(|error| error.kind())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        with_timeout(self.timeout, self.io.flush())
            .await
            .map_err(|_| ErrorKind::TimedOut)?
            .map_err(|error| error.kind())
    }
}
