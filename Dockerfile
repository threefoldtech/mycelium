FROM debian:latest AS build

# Copy across data
RUN mkdir /src
COPY . /src
