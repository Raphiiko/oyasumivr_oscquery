# oyasumivr_oscquery

Limited OSCQuery implementation in Rust, for use with VRChat. 

This library allows for easily advertising your OSC server to VRChat using OSCQuery.
**It is _not_ a full implementation of OSCQuery:** It _only_ implements the parts that are needed for interacting with VRChat.

Its main reason for existing is to add OSCQuery support to [OyasumiVR](https://github.com/Raphiiko/OyasumiVR).

Pull requests are welcome, however feature requests will likely go ignored, as this library is more of a personal means to and end.

## Credit

Big thanks to [góngo](https://github.com/TheMrGong) for figuring out the modifications required to the [mdns-sd](https://github.com/Raphiiko/vrc-mdns-sd) crate, for its advertisements to play nicely with VRChat.

Roughly based on specifications from the [OSCQuery Proposal](https://github.com/vrchat-community/osc/wiki/OSCQuery) and [VRChat's OSCQuery documentation](https://github.com/vrchat-community/osc/wiki/OSCQuery).

## Usage

Below you'll find some simple examples of how to use this library. For more detailed examples that you can run straight out of the box, please check the [examples](https://github.com/Raphiiko/oyasumivr_oscquery/tree/main/examples) directory.

### Include the library in your project

Add the following dependency to your `Cargo.toml`:

```toml
[dependencies]
oyasumivr_oscquery = { git = "https://github.com/Raphiiko/oyasumivr_oscquery.git" }
```

### Listen for data from VRChat

```rust
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
```

### Expose custom OSC Methods

Before you saw how to easily expose the right OSC methods for use with VRChat, by calling functions like `receive_vrchat_avatar_parameters` and `receive_vrchat_tracking_data`.

If you want to expose your own OSC methods, you can do so like follows:

```rust
// Initialize the OSCQuery server
oyasumivr_oscquery::init(...).await.unwrap();

// Set up which data we want to receive from VRChat
oyasumivr_oscquery::add_osc_method(OSCMethod {
    description: Some("VRChat Avatar Parameters".to_string()),
    address: "/avatar".to_string(),
    // Write: We only want to receive these values from VRChat, not send them
    ad_type: oyasumivr_oscquery::OSCAddressAdType::Write,
    value_type: None,
    value: None,
})
.await;

// Now we can start broadcasting the advertisement for the OSC and OSCQuery server
oyasumivr_oscquery::advertise().await.unwrap();
```

### Change the address of your OSC server

You can change the address of your OSC server at any time, even after you've already started (advertising) your OSCQuery server, by calling the `set_osc_server_address` function:

```rust
oyasumivr_oscquery::set_osc_address("127.0.0.1", 8082).await;
``````

### Expose OSC method values over OSCQuery

Although irrelevant for use with VRChat, with this library you can also expose values for your OSC methods over OSCQuery, so that they become queryable.

```rust
oyasumivr_oscquery::set_osc_method_value("/foo/bar".to_string(), Some("1".to_string())).await;
```

