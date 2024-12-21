### Load Balancer

# A distributed system load balancer in rust

General Concept:

* Clients can connect to the manager
* The manager is a round-robin DNS load balancer to multiple servers
* The manager redirects clients to a specific server that can handle another user
* The manager is fault-tolerant, meaning it properly handles cases where servers die
