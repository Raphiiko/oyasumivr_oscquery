use std::{
    net::{SocketAddrV4, UdpSocket},
    str::FromStr,
};

use oyasumivr_oscquery::models::OSCAddressAd;
use rosc::OscPacket;

#[tokio::main]
async fn main() {
    // Just start a simple OSC server that logs all received packets
    start_osc_server("127.0.0.1", 8081);
    // Start the OSCQuery server
    match oyasumivr_oscquery::init("OyasumiVRTest", "127.0.0.1", 8081).await {
        Ok(_) => println!("OyasumiVR OSCQuery server started!"),
        Err(e) => println!("Error starting OyasumiVR OSCQuery server: {:#?}", e),
    }
    // Set up the OSC Query server by registering addresses we're interesting in receiving
    oyasumivr_oscquery::add_osc_address_advertisement(OSCAddressAd {
        address: "/avatar".to_string(),
        ad_type: oyasumivr_oscquery::OSCAddressAdType::WriteAll,
        value_type: None,
        value: None,
        description: None,
    })
    .await;
    // Specific ones to try
    // oyasumivr_oscquery::add_osc_address_advertisement(OSCAddressAd {
    //     address: "/avatar/parameters/Grounded".to_string(),
    //     ad_type: oyasumivr_oscquery::OSCAddressAdType::WriteValue,
    //     value_type: None,
    //     value: None,
    //     description: Some("Grounded Avatar Parameter".to_string()),
    // })
    // .await;
    // oyasumivr_oscquery::add_osc_address_advertisement(OSCAddressAd {
    //     address: "/avatar/parameters/MuteSelf".to_string(),
    //     ad_type: oyasumivr_oscquery::OSCAddressAdType::WriteValue,
    //     value_type: None,
    //     value: None,
    //     description: Some("MuteSelf Avatar Parameter".to_string()),
    // })
    // .await;
    // Now start advertising the OSC and OSCQuery server
    oyasumivr_oscquery::start_advertising().await;
    // Keep process alive
    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}

fn start_osc_server(host: &str, port: u16) {
    let addr = SocketAddrV4::from_str(format!("{}:{}", host, port).as_str()).unwrap();
    std::thread::spawn(move || {
        let sock = UdpSocket::bind(addr).unwrap();
        println!("Listening for OSC packets on {}", addr);
        let mut buf = [0u8; rosc::decoder::MTU];

        loop {
            match sock.recv_from(&mut buf) {
                Ok((size, addr)) => {
                    println!("Received packet with size {} from: {}", size, addr);
                    let (_, packet) = rosc::decoder::decode_udp(&buf[..size]).unwrap();
                    handle_packet(packet);
                }
                Err(e) => {
                    println!("Error receiving from socket: {}", e);
                    break;
                }
            }
        }
    });
}

fn handle_packet(packet: OscPacket) {
    match packet {
        OscPacket::Message(msg) => {
            println!("OSC address: {}", msg.addr);
            println!("OSC arguments: {:?}", msg.args);
        }
        OscPacket::Bundle(bundle) => {
            println!("OSC Bundle: {:?}", bundle);
        }
    }
}
