# Development / test scripts

## `setup_network.sh`

`setup_network.sh` is used as-is, and as_root :-/
This little thing adds some LINUX network namespaces in which you can run mycelium 
U're a dev so deal with it


## testing mycelium (`bigmush.sh`)
This lill skrip will just start $NUMOFNS network namespaces in your LINUX box where you can SUDO (because I __know__ for a fact that you weren't root, of course), start a mycelium daemon in the main namespace and one in each NS.

### Usage :

Start with 
```bash
source ./bigmush.sh
getmycelium
```
will get the latest __release__ binary from github

Then
```bash
source ./bigmush.sh
doit
```

will create and start a 50 node mycelium with one central

```bash
source ./bigmush
dropit
```
will kill with little mercy mycelium daemons and delete the namespaces


```bash
source ./bigmush.sh
cleanit
```
will do a `dropit` and clean `*.{bin,out}` files

```bash
showit
```

will send a USR1 signal to all mycelium daemons that will
  - send routing tables and peers to stdout
  - where stdout will be captured in `xx.out` for each NS

### logging
every namespace has an `xx.out` file that is stout and stderr
the `xx.bin` file is the namespace daemon's privkey.

### behaviour testing

1) verify if you can reach all mycelium namespaces
2) also when running another machine in your net, verify if it's automatically detected
