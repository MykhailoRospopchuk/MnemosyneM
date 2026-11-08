use image_hasher::HasherConfig;

pub fn compute_hash(image_bytes: &[u8]) -> Result<u64, String> {
    let img = image::load_from_memory(image_bytes).map_err(|e| e.to_string())?;

    let hasher = HasherConfig::new().to_hasher();
    let hash = hasher.hash_image(&img);

    let butes = hash.as_bytes();
    if butes.len() == 8 {
        Ok(u64::from_be_bytes(butes.try_into().unwrap()))
    } else {
        Err("Unexpacted hash lenght".to_string())
    }
}
