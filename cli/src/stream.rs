use std::io::Read;
use std::io::Write;
use std::sync::mpsc::channel;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::thread;



use crate::bitwise::bitreader::BitReader;
use crate::bitwise::bitwriter::Bit;
use crate::bitwise::bitwriter::BitWriter;
use crate::bitwise::bitwriter::BitWriterImpl;
use crate::lib::block::block_data::generate_block_data;
use crate::lib::block::block_decoder::decode_block;
use crate::lib::stream::file_header;
use crate::lib::stream::stream_footer;

type Work = Vec<u8>;
type ComputationResult = (Vec<Bit>, u32);

struct WorkerThread {
    send_work: Sender<Work>,
    receive_result: Receiver<ComputationResult>,
    pending: bool,
}

impl WorkerThread {
    fn spawn(name: &str) -> Self {
        let (send_work, receive_work) = channel::<Work>();
        let (send_result, receive_result) = channel::<ComputationResult>();
        let builder = thread::Builder::new().name(name.into());

        builder
            .spawn(move || {
                while let Ok(work) = receive_work.recv() {
                    send_result.send(generate_block_data(&work)).unwrap();
                }
            })
            .unwrap();
        WorkerThread {
            send_work,
            receive_result,
            pending: false,
        }
    }

    fn flush_work_buffer(&mut self, mut bit_writer: impl BitWriter, total_crc: &mut u32) {
        let result = self.receive_result.recv().unwrap();

        bit_writer.write_bits(&result.0).unwrap();
        *total_crc = result.1 ^ ((*total_crc << 1) | (*total_crc >> 31));
        self.pending = false;
    }

    fn send_work(&mut self, work_to_send: Work) {
        self.pending = true;
        self.send_work.send(work_to_send).unwrap();
    }
}

pub fn write_stream_data(mut read: impl Read, mut writer: impl Write, num_threads: usize) {
    let mut bit_writer = BitWriterImpl::from_writer(&mut writer);
    // 900_000 * 4 / 5 - RLE can blow up 4chars to 5, hence we keep
    // a safety margin of 180,000
    const BLOCK_SIZE: usize = 720_000;
    let mut total_crc: u32 = 0;

    let mut worker_threads = (0..num_threads)
        .map(|num| WorkerThread::spawn(&format!("Thread {}", num)))
        .collect::<Vec<_>>();

    bit_writer.write_bits(&file_header()).unwrap();

    let mut finalize = false;
    loop {
        if finalize {
            break;
        }
        for worker_thread in worker_threads.iter_mut() {
            let mut buf = vec![];
            if let Ok(size) = read.by_ref().take(BLOCK_SIZE as u64).read_to_end(&mut buf) {
                if size == 0 {
                    finalize = true;
                    break;
                }
            } else {
                break;
            }
            worker_thread.send_work(buf);
        }

        for worker_thread in worker_threads.iter_mut() {
            if worker_thread.pending {
                worker_thread.flush_work_buffer(&mut bit_writer, &mut total_crc);
            }
        }
    }

    bit_writer.write_bits(&stream_footer(total_crc)).unwrap();
    bit_writer.finalize().unwrap();
}

fn read_file_header(mut bit_reader: impl BitReader) -> Result<(), ()> {
    let res = bit_reader.read_bytes(4)?;
    match &res[..] {
        [b'B', b'Z', b'h', _] => Ok(()),
        _ => {
            println!("Not a valid bz2 file");
            Err(())
        }
    }
}

#[derive(Debug, PartialEq)]
enum BlockType {
    StreamFooter,
    BlockHeader,
}

fn what_next(mut bit_reader: impl BitReader) -> Result<BlockType, ()> {
    let res = bit_reader.read_bytes(6)?;
    match &res[..] {
        [0x31u8, 0x41u8, 0x59u8, 0x26u8, 0x53u8, 0x59u8] => Ok(BlockType::BlockHeader),
        [0x17, 0x72, 0x45, 0x38, 0x50, 0x90] => Ok(BlockType::StreamFooter),
        _ => {
            println!("Expected block start or stream end");
            Err(())
        }
    }
}

pub(crate) fn write_stream(
    mut bit_reader: impl BitReader,
    mut writer: impl Write,
) -> Result<(), ()> {
    read_file_header(&mut bit_reader)?;
    loop {
        match what_next(&mut bit_reader)? {
            BlockType::StreamFooter => break,
            BlockType::BlockHeader => {
                decode_block(&mut bit_reader, &mut writer)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {

    use crate::bitwise::bitreader::BitReaderImpl;

    use super::*;
    use std::io::Cursor;

    #[test]
    pub fn accepts_correct_header() {
        let input = b"BZh9";
        let mut cursor = Cursor::new(input);
        let mut bit_reader = BitReaderImpl::from_reader(&mut cursor);
        let read = read_file_header(&mut bit_reader);
        assert!(read.is_ok());
    }

    #[test]
    pub fn detects_block_header() {
        let data = vec![0x31u8, 0x41u8, 0x59u8, 0x26u8, 0x53u8, 0x59u8];
        let mut cursor = Cursor::new(data);
        let mut bit_reader = BitReaderImpl::from_reader(&mut cursor);

        assert_eq!(BlockType::BlockHeader, what_next(&mut bit_reader).unwrap());
    }

    #[test]
    pub fn detects_stream_footer() {
        let data = vec![0x17, 0x72, 0x45, 0x38, 0x50, 0x90];
        let mut cursor = Cursor::new(data);
        let mut bit_reader = BitReaderImpl::from_reader(&mut cursor);

        assert_eq!(BlockType::StreamFooter, what_next(&mut bit_reader).unwrap());
    }

    #[test]
    pub fn detects_error() {
        let data = vec![0, 1, 2, 3, 4, 5];
        let mut cursor = Cursor::new(data);
        let mut bit_reader = BitReaderImpl::from_reader(&mut cursor);

        assert!(what_next(&mut bit_reader).is_err());
    }
}
