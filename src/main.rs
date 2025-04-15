use std::{net::{Ipv4Addr, SocketAddrV4}, io::{self, Read}, thread, time::{Duration, Instant}, collections::HashMap, fs};
use std::ffi::c_int;
use std::process::Command;
use std::thread::Thread;
use interfaces::Interface;
use socket2::{Domain, Protocol, Socket, Type};
use tun_tap::{Mode, Iface};
use serde::Deserialize;
use parking_lot::Mutex;
use hexhex::decode;


fn xor_encrypt(data: &[u8], key: &[u8]) -> Vec<u8> {
    data.iter()
        .zip(key.iter().cycle())
        .map(|(d, k)| d ^ k)
        .collect()
}

fn create_recv_socket(bind_ip: Ipv4Addr, protocol: u16) -> io::Result<Socket> {
    let socket = Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::from(protocol as c_int)))?;
    socket
        .bind(&(SocketAddrV4::new(bind_ip, protocol).into()))?;
    Ok(socket)
}

struct Wrapper<'a>(&'a Socket);

impl std::io::Read for Wrapper<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

fn add_fwd_tunnel(tunnels: &mut HashMap<u16, (Socket, Socket)>, id: u16, side_1: Ipv4Addr, side_2: Ipv4Addr, protocol: u16) -> io::Result<()> {
    let side_1_sock = Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::from(protocol as c_int)))?;
    side_1_sock
        .connect(&(SocketAddrV4::new(side_1, protocol).into()))?;
    let side_2_sock = Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::from(protocol as c_int)))?;
    side_2_sock
        .connect(&(SocketAddrV4::new(side_2, protocol).into()))?;
    tunnels.insert(id, (side_1_sock, side_2_sock));
    Ok(())
}

fn add_tap_tunnel(tunnels: &mut HashMap<u16, (Socket, Iface)>, id: u16, remote_ip: Ipv4Addr, iface: &str, tap_ip: &str, protocol: u16, preload_triggers: Vec<TriggerConfig>, postload_triggers: Vec<TriggerConfig>) -> String {
    let socket = Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::from(protocol as c_int))).unwrap();
    socket
        .connect(&(SocketAddrV4::new(remote_ip, protocol).into())).unwrap();
    let iface_name = iface.clone();
    //preload trigger
    for preload_trigger in preload_triggers {
        let cmd = preload_trigger.cmd
            .replace("{tap_ip}", tap_ip)
            .replace("{id}", id.to_string().as_str())
            .replace("{remote_ip}", remote_ip.to_string().as_str())
            .replace("{iface}", iface_name.clone())
            .replace("{protocol}", protocol.to_string().as_str());
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .expect("failed to preload trigger");
    }
    let iface = Iface::without_packet_info(iface, Mode::Tap).unwrap();
    let if_name = iface.name().to_string();
    tunnels.insert(id, (socket, iface));
    thread::sleep(Duration::from_millis(300));
    if !tap_ip.eq("") {
        println!("Setup ip {:?} on tap interface {:?}", tap_ip, iface_name);
        Command::new("ip")
            .arg("addr")
            .arg("add")
            .arg(tap_ip)
            .arg("dev")
            .arg(iface_name)
            .output()
            .expect("failed to set ip on tap");
    }
    //postload trigger
    thread::sleep(Duration::from_millis(200));
    for postload_trigger in postload_triggers {
        let cmd = postload_trigger.cmd
            .replace("{tap_ip}", tap_ip)
            .replace("{id}", id.to_string().as_str())
            .replace("{remote_ip}", remote_ip.to_string().as_str())
            .replace("{iface}", iface_name)
            .replace("{protocol}", protocol.to_string().as_str());
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .expect("failed to postload trigger");
    }
    if_name
}

#[derive(Debug, Deserialize)]
struct Config {
    general: GeneralConfig,
    #[serde(default)]
    tap_tunnels: Vec<TapTunnelConfig>,
    #[serde(default)]
    fwd_tunnels: Vec<FwdTunnelConfig>,
    #[serde(default)]
    preload_triggers: Vec<TriggerConfig>,
    #[serde(default)]
    postload_triggers: Vec<TriggerConfig>,
}

#[derive(Debug, Deserialize)]
struct GeneralConfig {
    idle_timeout: IdleIimeout,
    bind_ip: Ipv4Addr,
    protocol: u16,
    packet_header: String,
}

#[derive(Debug, Deserialize)]
struct IdleIimeout(u64);

#[derive(Debug, Deserialize, Clone)]
struct TriggerConfig {
    cmd: String,
}
#[derive(Debug, Deserialize)]
struct TapTunnelConfig {
    id: u16,
    remote_ip: Ipv4Addr,
    tap_ip: String,
    iface: String,
}

#[derive(Debug, Deserialize)]
struct FwdTunnelConfig {
    id: u16,
    source_ip: Ipv4Addr,
    destination_ip: Ipv4Addr,
}

#[derive(PartialEq, Eq)]
enum TunnelState {
    Down,
    Up {
        last_packet: Instant,
    }
}

impl TunnelState {
    fn got_packet(&mut self) -> bool {
        let came_up = *self == Self::Down;
        *self = Self::Up {
            last_packet: Instant::now(),
        };
        came_up
    }

    fn check_timeout(&mut self, idle_timeout: Duration) -> bool {
        if let Self::Up{last_packet} = self {
            if last_packet.elapsed() > idle_timeout {
                *self = Self::Down;
                return true
            }
        }
        false
    }
}

fn main() {
    let config = fs::read_to_string("config.toml").unwrap();
    let config: Config = toml::from_str(&config).unwrap();
    let idle_timeout = Duration::from_secs(config.general.idle_timeout.0);
    let bind_ip = config.general.bind_ip;
    let protocol = config.general.protocol;
    let packet_header = config.general.packet_header;
    let mut tap_tunnels: HashMap<u16, (Socket, Iface)> = HashMap::new();
    let mut fwd_tunnels: HashMap<u16, (Socket, Socket)> = HashMap::new();
    let mut tap_tunnel_states: HashMap<u16, TunnelState> = HashMap::new(); 
    let mut fwd_tunnel_states: HashMap<u16, (TunnelState, TunnelState)> = HashMap::new();

    for tap_tunnel in config.tap_tunnels {
        println!("Tunnel {}: {} <-> iface {}", tap_tunnel.id, tap_tunnel.remote_ip, tap_tunnel.iface);
        let name = add_tap_tunnel(&mut tap_tunnels, tap_tunnel.id, tap_tunnel.remote_ip, &tap_tunnel.iface, &tap_tunnel.tap_ip, protocol, config.preload_triggers.clone(), config.postload_triggers.clone());
        tap_tunnel_states.insert(tap_tunnel.id, TunnelState::Down);
        let mut iface = Interface::get_by_name(&name).unwrap().unwrap();
        iface.set_up(false).unwrap();
    }
    for fwd_tunnel in config.fwd_tunnels {
        println!("Tunnel {}: {} <-> {}", fwd_tunnel.id, fwd_tunnel.source_ip, fwd_tunnel.destination_ip);
        add_fwd_tunnel(&mut fwd_tunnels, fwd_tunnel.id, fwd_tunnel.source_ip, fwd_tunnel.destination_ip, protocol).unwrap();
        fwd_tunnel_states.insert(fwd_tunnel.id, (TunnelState::Down, TunnelState::Down));
    }
    let tap_tunnels = Box::leak(Box::new(tap_tunnels));
    let fwd_tunnels = Box::leak(Box::new(fwd_tunnels));
    let tap_tunnel_states = Box::leak(Box::new(Mutex::new(tap_tunnel_states)));
    let fwd_tunnel_states = Box::leak(Box::new(Mutex::new(fwd_tunnel_states)));
    let recv_socket = Box::leak(Box::new(create_recv_socket(bind_ip, protocol).unwrap()));

    let mut eoip_hdr = decode(&*packet_header.replace(" ", "")).unwrap();
    thread::scope(|s| {
        //send hello and idle timeout headers
        for (id, (socket, iface)) in &*tap_tunnels {
            eoip_hdr[6..8].copy_from_slice(&id.to_le_bytes());
            let eoip_hdr = Box::leak(eoip_hdr.clone().into_boxed_slice());
            s.spawn(|| {
                loop {
                    socket.send(eoip_hdr).unwrap();
                    thread::sleep(Duration::from_secs(10));
                }
            });
            s.spawn(|| {
                let mut buf = [0; 2048+8];
                buf[0..8].copy_from_slice(eoip_hdr);
                loop {
                    let len = iface.recv(&mut buf[8..]).unwrap();
                    socket.send(&buf[0..(len+8)]).unwrap();
                }
            });
        }
        //main thread
        s.spawn(|| {
            let mut buf = [0; 2048];
            loop {
                let len = Wrapper(recv_socket).read(&mut buf).unwrap();
                let id = u16::from_le_bytes(buf[26..28].try_into().unwrap());
                if let Some(tunnel_state) = tap_tunnel_states.lock().get_mut(&id) {
                    let iface = &tap_tunnels.get(&id).unwrap().1;
                    if tunnel_state.got_packet() {
                        println!("TAP tunnel {id} up");
                        let mut linux_iface = Interface::get_by_name(iface.name()).unwrap().unwrap();
                        linux_iface.set_up(true).unwrap();
                    }
                    if len == 28 {
                        // Got keepalive
                        continue;
                    }
                    let inner_packet = &mut buf[0..len][28..];
                    //encrypt inner_packet
                    let encrypted_data = xor_encrypt(&inner_packet, b"secret");
                    println!("{:x?}", inner_packet);
                    println!("{:x?}", encrypted_data);
                    println!("{:?}", "==================================");
                    iface.send(inner_packet).unwrap();
                }
                //tunnel proxy
                else if let Some((side_1_tunnel_state, side_2_tunnel_state)) = fwd_tunnel_states.lock().get_mut(&id) {
                    let src_ip = Ipv4Addr::from(<[u8; 4]>::try_from(&buf[12..16]).unwrap());
                    let (side_1_sock, side_2_sock) = fwd_tunnels.get(&id).unwrap();
                    let eoip_packet = &buf[0..len][20..];
                    if side_1_sock.peer_addr().unwrap().as_socket_ipv4().unwrap().ip() == &src_ip {
                        if side_1_tunnel_state.got_packet() {
                            println!("Forward tunnel {id} side 1 up");
                        }
                        side_2_sock.send(eoip_packet).unwrap();
                    } else {
                        if side_2_tunnel_state.got_packet() {
                            println!("Forward tunnel {id} side 2 up");
                        }
                        side_1_sock.send(eoip_packet).unwrap();
                    };
                } else {
                    println!("Unknown tunnel {id}");
                }
            };
        });
        s.spawn(|| {
            loop {
                for (id, tunnel_state) in &mut *tap_tunnel_states.lock() {
                    if tunnel_state.check_timeout(idle_timeout) {
                        println!("TAP tunnel {id} down");
                        let if_name = tap_tunnels.get(&id).unwrap().1.name();
                        Interface::get_by_name(&if_name).unwrap().unwrap().set_up(false).unwrap();
                    }
                }
                for (id, (side_1_tunnel_state, side_2_tunnel_state)) in &mut *fwd_tunnel_states.lock() {
                    if side_1_tunnel_state.check_timeout(idle_timeout) {
                        println!("Forward tunnel {id} side 1 down");
                    }
                    if side_2_tunnel_state.check_timeout(idle_timeout) {
                        println!("Forward tunnel {id} side 2 down");
                    }
                }
                thread::sleep(Duration::from_secs(10));
            }
        });
    });
}
