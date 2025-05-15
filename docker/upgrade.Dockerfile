FROM ubuntu:24.04

ARG TARGETARCH

RUN apt-get update \
    && apt-get install -y curl libcurl4 wait-for-it tini npm libusb-dev \
    && rm -rf /var/lib/apt/lists/*
ENTRYPOINT ["tini", "--"]
RUN npm install -g yarn && yarn global add typescript ts-node

WORKDIR /app

COPY package.json yarn.lock ./
RUN yarn

COPY contracts/script/multisigTransactionProposals/safeSDK ./contracts/script/multisigTransactionProposals/safeSDK/

COPY scripts/multisig-upgrade-entrypoint /bin/multisig-upgrade-entrypoint
COPY target/$TARGETARCH/release/deploy /bin/deploy

WORKDIR /
CMD [ "/bin/deploy"]
