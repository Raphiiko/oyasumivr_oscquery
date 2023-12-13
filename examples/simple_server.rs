mod osc_server;

#[tokio::main]
async fn main() {
    // Just start a simple OSC server that logs all received packets, for demonstration purposes.
    osc_server::start("0.0.0.0", 8081);

    // Initialize the OSCQuery server
    oyasumivr_oscquery::server::init(
        "OyasumiVR Test", // The name of your application (Shows in VRChat's UI)
        "127.0.0.1",      // The IP address your OSC server receives data on
        8081,             // The port your OSC server receives data on
    )
    .await
    .unwrap();

    // Configure which data we want to receive from VRChat
    oyasumivr_oscquery::server::receive_vrchat_avatar_parameters().await; // /avatar/*, /avatar/parameters/*, etc.
    oyasumivr_oscquery::server::receive_vrchat_tracking_data().await; // /tracking/vrsystem/*

    // Now we can start broadcasting the advertisement for the OSC and OSCQuery server
    oyasumivr_oscquery::server::advertise().await.unwrap();

    // Keep process alive
    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}
