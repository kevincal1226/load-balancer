<!--toc:start-->
# Load Balancer

## A distributed system load balancer in rust

- [General Concept](#general-concept)
- [Dependencies](#dependencies)
- [To Run](#to-run)
<!--toc:end-->

# General Concept

- Clients can connect to the manager
- The manager is a round-robin DNS load balancer to multiple servers
- The manager redirects clients to a specific server that can handle another user
- The manager is fault-tolerant, meaning it properly handles cases where servers die via UDP heartbeats

# Dependencies

- `netcat` (likely pre-installed on your machine)
- `rustc`/`cargo` >= 1.83.0

# To Run

- Use `./scripts/start <manager_host> <manager_port> <num_servers> <min_clients_per_server> <max_clients_per_server> <num_clients>` to create a manager on `<manager_host>:<manager_port>`, `<num_servers>` servers with a maximum number of clients in the range [`<min_clients_per_server>`, `<max_clients_per_server>`], and `<num_clients>` clients in the background

- Use `./scripts/send_shutdown` to shut down the manager, servers, and all clients

- Use `./scripts/create_server <manager_host> <manager_port> <server_host> <server_port> <max_num_clients>` to create a server in the background that can hold at most `<max_num_clients>` clients at a time

- Use `./scripts/create_client <manager_host> <manager_port> <client_host> <client_port>` to create a client in the background that will repeatedly connect and disconnect from the manager and receive redirection messages to different servers

- Use `./scripts/kill_random_server` to kill a random server and test how fault tolerance works
