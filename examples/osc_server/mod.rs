//
// Simple OSC server that logs all received packets
//

use std::{
    net::{SocketAddrV4, UdpSocket},
    str::FromStr,
};

use rosc::OscPacket;

pub fn start(host: &str, port: u16) {
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
