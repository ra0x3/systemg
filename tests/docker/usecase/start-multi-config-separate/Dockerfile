FROM sysg-usecase-base

COPY tests/docker/usecase/start-multi-config-separate/alpha.yaml /usecase/alpha.yaml
COPY tests/docker/usecase/start-multi-config-separate/beta.yaml /usecase/beta.yaml
COPY tests/docker/usecase/start-multi-config-separate/gamma.yaml /usecase/gamma.yaml
COPY tests/docker/usecase/start-multi-config-separate/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
