fn main() -> Result<(), Box<dyn std::error::Error>> {
    let devices = rustfus_windows::enumerate_usb_devices()?;
    println!("Rustfus discovered {} USB disk(s)", devices.len());
    for device in devices {
        println!(
            "disk={} instance={:?} model={:?} serial={:?} capacity={} logical={} physical={} eligible={} volumes={:?}",
            device.disk_number,
            device.instance_id,
            device.model,
            device.serial,
            device.capacity_bytes,
            device.logical_sector_size,
            device.physical_sector_size,
            device.is_eligible_usb_target(),
            device.volumes
        );
    }
    Ok(())
}
