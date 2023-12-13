mod osc_server;

#[tokio::main]
async fn main() {
    // Just start a simple OSC server that logs all received packets, for demonstration purposes.
    osc_server::start("0.0.0.0", 8081);

    // Initialize the OSCQuery server
    oyasumivr_oscquery::init(
        "OyasumiVR Test", // The name of your application (Shows in VRChat's UI)
        "127.0.0.1",      // The IP address your OSC server receives data on
        8081,             // The port your OSC server receives data on
    )
    .await
    .unwrap();

    // Set up which data we want to receive from VRChat
    oyasumivr_oscquery::receive_vrchat_avatar_parameters().await;
    oyasumivr_oscquery::receive_vrchat_tracking_data().await;

    // Now we can start broadcasting the advertisement for the OSC and OSCQuery server
    oyasumivr_oscquery::advertise().await.unwrap();

    // Keep process alive
    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}
