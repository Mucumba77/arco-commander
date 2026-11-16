// Expected digests generated independently with CPython hashlib/OpenSSL and zlib.
use super::*;
#[test]
fn checksums_match_independent_digest_vectors_for_all_standard_algorithms() {
    assert!(matches!(Hash::parse("SHA3-256"), Some(Hash::Sha3_256)));
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("reference.txt");
    fs::write(&path, b"abc").unwrap();
    let cancel = AtomicBool::new(false);
    for (algorithm, expected) in [
        (Hash::Crc32, "352441c2"),
        (Hash::Md5, "900150983cd24fb0d6963f7d28e17f72"),
        (Hash::Sha1, "a9993e364706816aba3e25717850c26c9cd0d89d"),
        (
            Hash::Sha224,
            "23097d223405d8228642a477bda255b32aadbce4bda0b3f7e36c9da7",
        ),
        (
            Hash::Sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            Hash::Sha384,
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7",
        ),
        (
            Hash::Sha512,
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        ),
        (
            Hash::Sha3_224,
            "e642824c3f8cf24ad09234ee7d3c766fc9a3a5168d0c94ad73b46fdf",
        ),
        (
            Hash::Sha3_256,
            "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532",
        ),
        (
            Hash::Sha3_384,
            "ec01498288516fc926459f58e2c6ad8df9b473cb0fc08c2596da7cf0e49be4b298d88cea927ac7f539f1edf228376d25",
        ),
        (
            Hash::Sha3_512,
            "b751850b1a57168a5693cd924b6b096e08f621827444f70d884f5d0240d2712e10e116e9192af3c91a7ec57647e3934057340b4cf408d5a56592f8274eec53f0",
        ),
    ] {
        let digest = checksum(&path, algorithm, &cancel).unwrap();
        assert_eq!(digest, expected);
        assert!(algorithm.matches_text(&digest));
        assert!(checksum(&path, algorithm, &AtomicBool::new(true)).is_err());
    }
}

#[test]
fn blake3_matches_the_upstream_empty_input_vector() {
    // https://github.com/BLAKE3-team/BLAKE3/blob/master/test_vectors/test_vectors.json
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("empty");
    fs::write(&path, []).unwrap();
    assert_eq!(
        checksum(&path, Hash::Blake3, &AtomicBool::new(false)).unwrap(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
}
