#![cfg(feature = "local-engine")]
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)] // Integration regressions report the exact failing tensor operation.

//! Public Candle byte APIs used by Susi model loading and alpha checkpoints.
//! These tests run in workspace gates, rather than only inside a dependency.

use susi_vendor_candle::candle_core::{safetensors, DType, Device, Tensor};
use susi_vendor_candle::candle_nn::{Init, VarMap};

fn unaligned_bytes(payload: &[u8]) -> (Vec<u8>, usize) {
    let mut storage = vec![0u8; payload.len() + 16];
    let offset = (8 - storage.as_ptr() as usize % 8) % 8 + 1;
    storage[offset..offset + payload.len()].copy_from_slice(payload);
    (storage, offset)
}

#[test]
fn partial_elements_are_rejected_at_aligned_and_unaligned_addresses() {
    let storage = [0x5au8; 64];
    for dtype in [
        DType::U32,
        DType::I16,
        DType::I32,
        DType::I64,
        DType::BF16,
        DType::F16,
        DType::F32,
        DType::F64,
    ] {
        let width = dtype.size_in_bytes();
        let aligned = (width - storage.as_ptr() as usize % width) % width;
        for offset in [aligned, aligned + 1] {
            for length in [1, width - 1, width + 1, 2 * width - 1] {
                let bytes = &storage[offset..offset + length];
                // In particular F32 + one unaligned byte must return an error
                // before any typed allocation/copy, even with a zero shape.
                assert!(
                    Tensor::from_raw_buffer(bytes, dtype, &[length / width], &Device::Cpu).is_err(),
                    "accepted partial {dtype:?} element at offset {offset}, length {length}"
                );
            }
        }
    }
}

#[test]
fn byte_count_and_shape_overflow_fail_closed_but_empty_and_scalar_shapes_work() {
    assert!(Tensor::from_raw_buffer(&[0; 8], DType::F32, &[1], &Device::Cpu).is_err());
    assert!(Tensor::from_raw_buffer(&[0; 4], DType::F32, &[2], &Device::Cpu).is_err());
    assert!(Tensor::from_raw_buffer(&[], DType::F64, &[usize::MAX, 2], &Device::Cpu).is_err());
    assert!(Tensor::from_raw_buffer(&[], DType::F64, &[usize::MAX], &Device::Cpu).is_err());
    assert!(Tensor::from_raw_buffer(&[], DType::F32, &[], &Device::Cpu).is_err());
    let empty = Tensor::from_raw_buffer(&[], DType::F64, &[0], &Device::Cpu).unwrap();
    assert!(empty.to_vec1::<f64>().unwrap().is_empty());
    let scalar =
        Tensor::from_raw_buffer(&(-2.5f32).to_le_bytes(), DType::F32, &[], &Device::Cpu).unwrap();
    assert_eq!(scalar.to_scalar::<f32>().unwrap(), -2.5);
}

#[test]
fn unaligned_little_endian_buffers_decode_every_numeric_dtype() {
    let cases: Vec<(DType, Vec<u8>, Vec<f64>)> = vec![
        (DType::U8, vec![0, 255], vec![0., 255.]),
        (
            DType::U32,
            [0x01020304u32, u32::MAX]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
            vec![16909060., 4294967295.],
        ),
        (
            DType::I16,
            [i16::MIN, 0x1234]
                .into_iter()
                .flat_map(i16::to_le_bytes)
                .collect(),
            vec![-32768., 4660.],
        ),
        (
            DType::I32,
            [i32::MIN, i32::MAX]
                .into_iter()
                .flat_map(i32::to_le_bytes)
                .collect(),
            vec![-2147483648., 2147483647.],
        ),
        (
            DType::I64,
            [-1099511627776i64, 0x12345678]
                .into_iter()
                .flat_map(i64::to_le_bytes)
                .collect(),
            vec![-1099511627776., 305419896.],
        ),
        (DType::BF16, vec![0x80, 0x3f, 0x20, 0xc0], vec![1., -2.5]),
        (DType::F16, vec![0x00, 0x3c, 0x00, 0xc1], vec![1., -2.5]),
        (
            DType::F32,
            [1.0f32, -2.5]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
            vec![1., -2.5],
        ),
        (
            DType::F64,
            [1.0f64, -2.5]
                .into_iter()
                .flat_map(f64::to_le_bytes)
                .collect(),
            vec![1., -2.5],
        ),
        (DType::F8E4M3, vec![0x38, 0xc2], vec![1., -2.5]),
    ];
    for (dtype, payload, expected) in cases {
        let (storage, offset) = unaligned_bytes(&payload);
        let bytes = &storage[offset..offset + payload.len()];
        if dtype.size_in_bytes() > 1 {
            assert_ne!(bytes.as_ptr() as usize % dtype.size_in_bytes(), 0);
        }
        let tensor = Tensor::from_raw_buffer(bytes, dtype, &[2], &Device::Cpu).unwrap();
        drop(storage);
        assert_eq!(
            tensor
                .to_dtype(DType::F64)
                .unwrap()
                .to_vec1::<f64>()
                .unwrap(),
            expected
        );
    }
}

#[test]
fn serialized_numeric_bit_patterns_survive_owned_save_load_and_drop() {
    let cases: Vec<(DType, Vec<u8>)> = vec![
        (DType::U8, vec![0, 0xff]),
        (
            DType::U32,
            [0x12345678u32, u32::MAX]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
        ),
        (
            DType::I16,
            [-1234i16, i16::MIN]
                .into_iter()
                .flat_map(i16::to_le_bytes)
                .collect(),
        ),
        (
            DType::I32,
            [-1i32, i32::MIN]
                .into_iter()
                .flat_map(i32::to_le_bytes)
                .collect(),
        ),
        (
            DType::I64,
            [-1i64, i64::MIN]
                .into_iter()
                .flat_map(i64::to_le_bytes)
                .collect(),
        ),
        (DType::BF16, vec![0, 0x80, 0xc1, 0x7f]),
        (DType::F16, vec![0, 0x80, 0x55, 0x7e]),
        (
            DType::F32,
            [0x80000000u32, 0x7fc12345]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
        ),
        (
            DType::F64,
            [0x8000000000000000u64, 0x7ff8123456789abc]
                .into_iter()
                .flat_map(u64::to_le_bytes)
                .collect(),
        ),
        (DType::F8E4M3, vec![0x80, 0x7f]),
    ];
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tensor.safetensors");
    for (dtype, payload) in cases {
        let tensor = Tensor::from_raw_buffer(&payload, dtype, &[2], &Device::Cpu).unwrap();
        tensor.save_safetensors("weights", &path).unwrap();
        drop(tensor);
        let saved = safetensors::BufferedSafetensors::new(std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.get("weights").unwrap().data(), payload, "{dtype:?}");
        let loaded = saved.load("weights", &Device::Cpu).unwrap();
        assert_eq!(loaded.dtype(), dtype);
        drop(saved);
        loaded.save_safetensors("weights", &path).unwrap();
        drop(loaded);
        let saved_again =
            safetensors::BufferedSafetensors::new(std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved_again.get("weights").unwrap().data(),
            payload,
            "{dtype:?}"
        );
    }
}

#[test]
fn unaligned_u16_safetensors_casts_to_u32_without_losing_values() {
    let mut header = r#"{"weights":{"dtype":"U16","shape":[3],"data_offsets":[0,6]}}"#.to_owned();
    if !header.len().is_multiple_of(2) {
        header.push(' ');
    }
    let mut wire = (header.len() as u64).to_le_bytes().to_vec();
    wire.extend_from_slice(header.as_bytes());
    wire.extend(
        [0u16, 0x1234, u16::MAX]
            .into_iter()
            .flat_map(u16::to_le_bytes),
    );
    assert!(safetensors::SliceSafetensors::new(&wire[..wire.len() - 1]).is_err());
    let (storage, offset) = unaligned_bytes(&wire);
    let owner = safetensors::SliceSafetensors::new(&storage[offset..offset + wire.len()]).unwrap();
    assert_ne!(
        owner.get("weights").unwrap().data().as_ptr() as usize % 2,
        0
    );
    let tensor = owner.load("weights", &Device::Cpu).unwrap();
    assert_eq!(tensor.dtype(), DType::U32);
    assert_eq!(tensor.to_vec1::<u32>().unwrap(), [0, 0x1234, 65535]);
}

#[test]
fn alpha_varmap_checkpoint_path_roundtrips_float_weights() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("alpha-checkpoint.safetensors");
    let mut variables = VarMap::new();
    variables
        .get((2, 3), "weights", Init::Const(0.), DType::F32, &Device::Cpu)
        .unwrap();
    let expected = [[1.0f32, -2.5, 3.25], [0., 100., -0.125]];
    variables
        .set_one("weights", Tensor::new(&expected, &Device::Cpu).unwrap())
        .unwrap();
    for _ in 0..8 {
        variables.save(&path).unwrap();
        let checkpoint = safetensors::load(&path, &Device::Cpu).unwrap();
        assert_eq!(checkpoint["weights"].to_vec2::<f32>().unwrap(), expected);
    }
    let mut restored = VarMap::new();
    restored
        .get((2, 3), "weights", Init::Const(0.), DType::F32, &Device::Cpu)
        .unwrap();
    restored.load(&path).unwrap();
    let actual = restored
        .get((2, 3), "weights", Init::Const(0.), DType::F32, &Device::Cpu)
        .unwrap();
    assert_eq!(actual.to_vec2::<f32>().unwrap(), expected);
}
