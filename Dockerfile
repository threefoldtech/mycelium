FROM debian:latest AS build

# TODO: Add nonnteractive flag for apt

# Upgrade system
RUN apt update
RUN apt upgrade -y

# Install build dependencies
RUN apt install rust -y

# Copy across data
RUN mkdir /src
COPY . /src

