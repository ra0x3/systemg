FROM sysg-usecase-base

COPY tests/docker/usecase/start-config-then-add/first.yaml /usecase/first.yaml
COPY tests/docker/usecase/start-config-then-add/second.yaml /usecase/second.yaml
COPY tests/docker/usecase/start-config-then-add/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
