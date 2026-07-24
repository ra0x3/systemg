FROM sysg-usecase-base

COPY tests/docker/usecase/start-stopped-manifest-change/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/start-stopped-manifest-change/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
