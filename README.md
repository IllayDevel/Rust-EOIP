# eoip
A Rust implementation of the EoIP (Ethernet Over IP) protocol, compatible with MikroTik RouterOS.
This implementation supports connections between Client -> MikroTik and Client -> Client.
The implementation also includes EoIP repeater functionality, allowing traffic to be forwarded from one tunnel to another.
All settings are located in the config.toml file. Here is an example of this file.

    [general]
    idle_timeout = 100
    bind_ip = "0.0.0.0"
    protocol = 47
    packet_header = "20 01 64 00 00 00 00 00"
    
    preload_triggers = []
    
    [[postload_triggers]]
    cmd = "echo {remote_ip} > ddd.ccf"
    
    [[postload_triggers]]
    cmd = "echo 1> ccf.ddd"
    
    [[tap_tunnels]]
    id = 16587
    remote_ip = "192.168.100.1"
    iface = "eoip-test"
    tap_ip = ""
    
    fwd_tunnels = []
 
Section [general] - Application main settings.
1. idle_timeout - Tunnel timeout.
2. bind_ip - Listening IP address.
3. protocol - Used protocol (Just for fun).
4. packet_header - Packet header. If you change this value, you will not be able to connect to MikroTik, only client-to-client connections will work. Changing this value might be necessary if your ISP blocks EoIP traffic.

Array [[tap_tunnels]] - Array of EoIP tunnels.
1. id - Unique tunnel ID.
2. remote_ip - IP address of the remote side of the tunnel.
3. iface - Name of the TAP interface.
4. tap_ip - IP address of the TAP interface. If necessary, specify a value, for example, 192.168.100.1.

Array [[fwd_tunnels]] - Array of tunnel forwarding rules. This feature is used when you cannot directly access the tunnel's destination, for example, if one end of the tunnel is on the Internet and the other is in a corporate network.
1. id - Unique tunnel ID.
2. source_ip - Source.
3. destination_ip - Destination.

Arrays [[preload_triggers]] and [[postload_triggers]]. Triggers executed before and after the creation of the TAP interface. This is an experimental feature and is currently only supported on Linux operating systems.
1. cmd - Command-line commands.
