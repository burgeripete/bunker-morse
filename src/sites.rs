//! Where bunkers can be. A broadcast only ever names one of these, which lets
//! the decoder recognise a block it joined partway through.

/// The coordinates of every bunker on Nasdara, as the broadcast sends them.
pub const NASDARA_BUNKERS: [&str; 23] = [
    "072106", "091044", "112067", "067058", "107123", "076076", "114053", "104083", "045054",
    "100121", "109086", "073090", "105102", "088112", "085115", "087088", "133021", "038066",
    "065090", "096109", "028100", "057074", "067124",
];
