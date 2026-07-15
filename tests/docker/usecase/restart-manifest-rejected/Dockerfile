FROM sysg-usecase-base

COPY tests/docker/usecase/restart-manifest-rejected/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/restart-manifest-rejected/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
