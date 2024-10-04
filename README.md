# oyasumivr_oscquery

Limited OSCQuery implementation in Rust, for use with VRChat.

This library allows for:

1. Advertising your OSC server to VRChat using OSCQuery. (For receiving OSC data from VRChat)
2. Finding VRChat's own OSC and OSCQuery servers. (For sending OSC data to VRChat)

**It is _not_ a full implementation of OSCQuery:** It _only_ implements the parts that are needed for interacting with VRChat. It does not handle the sending and receiving of OSC packets, for that you'll need to use a crate like [rosc](https://crates.io/crates/rosc).

Its main reason for existence is to add OSCQuery support to [OyasumiVR](https://github.com/Raphiiko/OyasumiVR).
Pull requests are welcome, however feature requests will likely go ignored, as this library is more of a personal means to an end.

Roughly based on specifications from the [OSCQuery Proposal](https://github.com/vrchat-community/osc/wiki/OSCQuery) and [VRChat's OSCQuery documentation](https://github.com/vrchat-community/osc/wiki/OSCQuery).

## Usage

Below you'll find some simple examples of how to use this library. For more detailed examples that you can run straight out of the box, please check the [examples](https://github.com/Raphiiko/oyasumivr_oscquery/tree/main/examples) directory.

### Include the library in your project

Add the following dependency to your `Cargo.toml`:

```toml
[dependencies]
oyasumivr_oscquery = { git = "https://github.com/Raphiiko/oyasumivr_oscquery.git" }
```

### Include the sidecar executable in your project

This library depends on a dotnet based sidecar executable that needs to be included with your project. You can find it by:
1. Downloading the [built executable](https://github.com/Raphiiko/oyasumivr_oscquery/blob/dotnet_sidecar/lib/mdns-sidecar.exe) from this repository

or

2. Building it yourself by cloning this repository and running `./build.sh`. The executable will end up in `lib/mdns-sidecar.exe`.

You will need to ship this executable with your project, with the rest of your program's files. The upcoming examples will show you how to refer to it.

### Sending (to VRChat)

#### Find VRChat's OSC and OSCQuery servers

You can find the addresses for VRChat's OSC and OSCQuery servers as follows.
Note that these can only be found while VRChat is running.
When VRChat is restarted or OSC is disabled/enabled, these addresses (and ports especially) _may_ change, so make sure to check these functions for changes regularly.

```rust
// Start looking for VRChat's OSC & OSCQuery services.
oyasumivr_oscquery::client::init(
    "./lib/mdns-sidecar.exe" // The (relative) path to the mdns-sidecar.exe executable
).await.unwrap();

// Wait a bit for the MDNS daemon to find the services
tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;

// Get the address of the VRChat OSC server
let (host, port) = oyasumivr_oscquery::client::get_vrchat_osc_address()
    .await
    .unwrap();
println!("VRChat OSC address: {}:{}", host, port);

// Get the address of the VRChat OSCQuery server
let (host, port) = oyasumivr_oscquery::client::get_vrchat_oscquery_address()
    .await
    .unwrap();
println!("VRChat OSC Query address: {}:{}", host, port);
```

### Receiving (from VRChat)

#### Listen for data from VRChat

```rust
// Initialize the OSCQuery server
oyasumivr_oscquery::server::init(
    "OyasumiVR Test",         // The name of your application (Shows in VRChat's UI)
    8085,                     // The port your OSC server receives data on
    "./lib/mdns-sidecar.exe", // The (relative) path to the MDNS sidecar executable
).await.unwrap();

// Configure which data we want to receive from VRChat
oyasumivr_oscquery::server::receive_vrchat_avatar_parameters().await; // /avatar/*, /avatar/parameters/*, etc.
oyasumivr_oscquery::server::receive_vrchat_tracking_data().await; // /tracking/vrsystem/*

// Now we can start broadcasting the advertisement for the OSC and OSCQuery server
oyasumivr_oscquery::server::advertise().await.unwrap();
```

#### Expose custom OSC Methods

Before you saw how to easily expose the right OSC methods for use with VRChat, by calling functions like `receive_vrchat_avatar_parameters` and `receive_vrchat_tracking_data`.

If you want to expose your own OSC methods, you can do so like follows:

```rust
// Initialize the OSCQuery server
oyasumivr_oscquery::server::init(...).await.unwrap();

// Configure the OSC Query server by registering addresses we're interesting in receiving
// Getting VRChat avatar parameters
oyasumivr_oscquery::server::add_osc_method(OSCMethod {
    description: Some("VRChat Avatar Parameters".to_string()),
    address: "/avatar".to_string(),
    // Write: We only want to receive these values from VRChat, not send them
    ad_type: oyasumivr_oscquery::OSCMethodAccessType::Write,
    value_type: None,
    value: None,
})
.await;

// Now we can start broadcasting the advertisement for the OSC and OSCQuery server
oyasumivr_oscquery::server::advertise().await.unwrap();
```

#### Change the advertised port of your OSC server

If you change the address your OSC server runs on, you can update the advertised port at any time, even after you've already started (advertising) your OSC & OSCQuery servers.
You can do this by calling the `set_osc_port` function:

```rust
oyasumivr_oscquery::server::set_osc_port(8082).await;
```

The advertisements will automatically be updated!

#### Expose OSC method values over OSCQuery

Although irrelevant for use with VRChat, with this library you can also expose values for your OSC methods over OSCQuery, so that they become queryable.

```rust
oyasumivr_oscquery::server::set_osc_method_value("/foo/bar".to_string(), Some("1".to_string())).await;
```
