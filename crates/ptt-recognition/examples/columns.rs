fn main() {
    let mut a = std::env::args().skip(1);
    let path = std::path::PathBuf::from(a.next().unwrap());
    let from: usize = a.next().unwrap().parse().unwrap();
    let to: usize = a.next().unwrap().parse().unwrap();
    let region = ptt_vision::CaptureRegion {
        x: 1163,
        y: 217,
        width: 269,
        height: 523,
    };
    let frame = ptt_vision::WicScreenshotDecoder::new()
        .unwrap()
        .decode(&path, Some(region))
        .unwrap();
    let mask = ptt_vision::build_warm_mask(&frame, ptt_vision::WarmMaskSettings::default());
    for y in from..=to.min(mask.height() - 1) {
        let cols: Vec<usize> = (0..mask.width())
            .filter(|&x| mask.intensity_at(x, y).unwrap_or(0) != 0)
            .collect();
        match (cols.first(), cols.last()) {
            (Some(&lo), Some(&hi)) => println!("row {y:3} n={:3} x {lo:3}-{hi:3}", cols.len()),
            _ => println!("row {y:3} n=  0"),
        }
    }
}
