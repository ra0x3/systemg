FROM sysg-usecase-base

COPY tests/docker/usecase/status-service-ambiguity/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/status-service-ambiguity/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
