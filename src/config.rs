use std::time::Duration;

// One of the Ethereum Foundation Geth Bootnodes
pub const BOOTNODE: &str = "enr:-Ku4QG-2_Md3sZIAUebGYT6g0SMskIml77l6yR-M_JXc-UdNHCmHQeOiMLbylPejyJsdAPsTHJyjJB2sYGDLe0dn8uYBh2F0dG5ldHOIAAAAAAAAAACEZXRoMpC1MD8qAAAAAP__________gmlkgnY0gmlwhBLY-NyJc2VjcDI1NmsxoQORcM6e19T1T9gi7jxEZjk_sjVLGFscUNqAY9obgZaxbIN1ZHCCIyg";

pub const GOSSIP_MAX_SIZE_BELLATRIX: usize = 10 * 1_048_576; // 10 * 2**20

pub const DUPLICATE_CACHE_TIME: Duration = Duration::from_secs(33 * 12 + 1);

// https://eth2book.info/bellatrix/part3/containers/state/#table_0

// BELLATRIX_FORK_VERSION = 0x02000000
pub const BELLATRIX_FORK_VERSION: [u8; 4] = [0x02, 0x00, 0x00, 0x00];

// GENESIS_VALIDATORS_ROOT = 0x4b363db94e286120d76eb905340fdd4e54bfe9f06bf33ff6cf5ad27f511bfe95
pub const GENESIS_VALIDATORS_ROOT: [u8; 32] = [
    75, 54, 61, 185, 78, 40, 97, 32, 215, 110, 185, 5, 52, 15, 221, 78, 84, 191, 233, 240, 107,
    243, 63, 246, 207, 90, 210, 127, 81, 27, 254, 149,
];
