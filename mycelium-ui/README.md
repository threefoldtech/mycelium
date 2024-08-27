# Mycelium Network Dashboard

The Mycelium Network Dashboard is a GUI application built with Dioxus, a modern library for building
cross-platform applications using Rust. More information about Dioxus can be found [here](https://dioxuslabs.com/)

## Getting Started

To get started with the Mycelium Network Dashboard, you'll need to have the Dioxus CLI tool installed.
You can install it using the following command:

`cargo install dioxus-cli`

Before running the Mycelium Network Dashboard application, make sure that the `myceliumd` daemon is running on your system. 
The myceliumd daemon is the background process that manages the Mycelium network connection 
and provides the data that the dashboard application displays. For more information on setting up and
running `myceliumd`, please read [this](../README.md).

Once you have the Dioxus CLI installed, you can build and run the application in development mode using
the following command (in the `mycelium-ui` directory):

`dx serve`

This will start a development server and launch the application in a WebView.

## Bundling the application

To bundle the application, you can use:

`dx bundle --release --features bundle`

This will create a bundled version of the application in the `dist/bundle/` directory. The bundled
application can be distributed and run on various platforms, including Windows, MacOS and Linux. Dioxus
also offers support for mobile, but note that this has not been tested.

## Documentation

The Mycelium Network Dashboard application provides the following features:

- **Home**: Displays information about the node and allows to change address of the API server on which
the application should listen.
- **Peers**: Shows and overview of all the connected peers. Adding and removing peers can be done here.
- **Routes**: Provides information about the routing table and network routes


## Contributing

If you would like to contribute to the Mycelium Network Dashboard project, please follow the standard GitHub workflow:

1. Fork the repository
2. Create a new branch for your changes
3. Make your changes and commit them
4. Push your changes to your forked repository
5. Submit a pull request to the main repository

