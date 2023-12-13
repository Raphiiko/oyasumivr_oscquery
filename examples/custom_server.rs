mod osc_server;

use oyasumivr_oscquery::models::OSCMethod;

#[tokio::main]
async fn main() {
    // Just start a simple OSC server that logs all received packets, for demonstration purposes.
    osc_server::start("0.0.0.0", 8081);

    // Initialize the OSCQuery server
    oyasumivr_oscquery::init("OyasumiVR Test", "127.0.0.1", 8081)
        .await
        .unwrap();

    // Set up the OSC Query server by registering addresses we're interesting in receiving
    oyasumivr_oscquery::add_osc_method(OSCMethod {
        description: Some("VRChat Avatar Parameters".to_string()),
        address: "/avatar".to_string(),
        // Write: We only want to receive these values from VRChat, not send them
        ad_type: oyasumivr_oscquery::OSCAddressAdType::Write,
        value_type: None,
        value: None,
    })
    .await;

    oyasumivr_oscquery::set_osc_address("127.0.0.1", 8082).await;

    // Also getting VR tracking data
    oyasumivr_oscquery::add_osc_method(OSCMethod {
        description: Some("VRChat VR Tracking Data".to_string()),
        address: "/tracking/vrsystem".to_string(),
        // Write: We only want to receive these values from VRChat, not send them
        ad_type: oyasumivr_oscquery::OSCAddressAdType::Write,
        value_type: None,
        value: None,
    })
    .await;

    // Now we can start broadcasting the advertisement for the OSC and OSCQuery server
    oyasumivr_oscquery::advertise().await.unwrap();

    // Keep process alive
    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}
