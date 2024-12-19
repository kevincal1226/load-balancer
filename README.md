### Load Balancer
# A distributed system load balancer in rust.


General Concept:
* Clients can connect to the server
* The server is a round-robin DNS load balancer to multiple servers
* The server redirects clients to a specific server that can handle another user
* The server is fault-tolerant, meaning it properly handles cases where servers die
