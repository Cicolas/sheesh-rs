# sheesh-rs / aiassh / shaia

features a serem incluidas:
- multiple connections
- SHEESH.md
- context hydratation
- sudo mode

### Multiple connections
multiple connections enable the LLM to access multiple ssh connections and interop
commands between then, this can be used to:
- Configure multiple machines at one command
- Context Hydration

### SHEESH.md
structure all the log of what has been done, how the server is structured and so on

### context hydration
while sheesh perform commands in the connections it should create entries and manipulate a
context folder called .sheesh/

### sudo mode
should be able to not crash once sudo is requested

### MCPs
sheesh shuold have acces to MCPs that can:
- Run a command
- Read a file in the system
- Create a file

### Terminal issues
- should be able to use keyboards that not ANSI
- should be able to draw and interact to complex flows in the ssh connection
  - as render vim; helix; lazy-docker

### Master / Slave mode
sheesh should have a master slave mode where you can install a process on the
server, then sheesh can connect through an cryptographic connection to perform
actions more efficiently, such as:
- create files
- read files
- and more powerups

### Things to have a look
- does open claw already do?
- does people would use it (market fit)
- how to monetize this?
  - master / slave mode should be paid?
  - multiple connections should be paid?